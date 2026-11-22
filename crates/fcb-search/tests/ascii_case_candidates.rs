#![forbid(unsafe_code)]

//! FCB-027.B: ASCII capture proofs let normalized queries reuse raw grams
//! without borrowing negative certificates across incompatible representations.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_search::{EphemeralIndex, IndexLimits, ManifestLimits, MembershipState,
    ParsedQuery, QueryOptions, ReferenceScanOracle, SearchDocument, SearchManifest,
    SearchManifestId, SearchMode, UnicodeNormalization};
use fcb_search::index::export::{OwnedIndexBuilder, SourceRetentionLimits};
use fcb_search::indexed_query::OwnedIndexedQuery;
use fcb_source::{CaptureRequest, CompleteCapture, DetectedEncoding};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(29042).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap()
}
fn capture(id: u64, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), id).unwrap(),
        SourceRevision::new(owner(), id).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn document<'a>(capture: &'a CompleteCapture) -> SearchDocument<'a> {
    SearchDocument::new(capture.request().file(), "source.rs", capture)
}
fn build_index<'a>(documents: &'a [SearchDocument<'a>], limits: IndexLimits,
    budget: &ResourceBudget) -> EphemeralIndex<'a> {
    let manifest = SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(),
        documents, &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    EphemeralIndex::build(manifest, limits, budget, allocation(1), || false).unwrap()
}
fn options(mode: SearchMode) -> QueryOptions {
    QueryOptions::new(generation()).with_mode(mode).with_max_matches(100)
}
fn insensitive_modes() -> [SearchMode; 4] {
    [
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact },
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::CaseFold },
        // CaseFold intentionally folds even when the independent case flag is true.
        SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::CaseFold },
        SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Canonical },
    ]
}
fn case_variant(mask: u8) -> String {
    String::from_utf8([b'a', b'b', b'c'].into_iter().enumerate()
        .map(|(offset, byte)| if mask & (1 << offset) == 0 { byte } else { byte.to_ascii_uppercase() })
        .collect()).unwrap()
}

#[test]
fn all_case_variants_and_nonletters_preserve_exact_ascii_hits() {
    for mask in 0..8 {
        let text = format!("{} a_1 123 X\r\nY\tZ", case_variant(mask));
        let source = capture(1, text.as_bytes());
        let documents = [document(&source)];
        let budget = budget();
        let index = build_index(&documents, IndexLimits::default(), &budget);
        let mut needles: Vec<_> = (0..8).map(case_variant).collect();
        needles.extend(["a_1", "A_1", "123"].map(str::to_owned));
        for mode in insensitive_modes() {
            for needle in &needles {
                let query = ParsedQuery::parse(&format!("\"{needle}\"")).unwrap();
                let opts = options(mode);
                assert!(index.may_match(0, &query, &opts).unwrap(), "{text:?}, {needle:?}, {mode:?}");
                let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
                let report = index.search(&query, opts, &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
                assert_eq!(report.capture_results().match_count(), 1);
                assert_eq!(report.fallback_attempts(), 0);
            }
            for needle in ["abd", "a_2", "124"] {
                assert!(!index.may_match(0, &ParsedQuery::parse(needle).unwrap(), &options(mode)).unwrap());
            }
        }
        // The exported/persisted representation remains raw byte grams.
        let mut raw: Vec<_> = text.as_bytes().windows(3)
            .map(|bytes| u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]])).collect();
        raw.sort_unstable();
        raw.dedup();
        assert_eq!(index.segment_image(0).unwrap().grams(), raw);
    }
}

#[test]
fn positive_terms_exclusions_short_needles_and_hit_limits_match_the_direct_oracle() {
    let captures = [capture(1, b"KeLvIn ALLOW baNANA abcXbcd"),
        capture(2, b"kelvin WITHOUT permission banana"), capture(3, b"KeLvIn ALLOW blocked"),
        capture(4, b"")];
    let documents: Vec<_> = captures.iter().map(document).collect();
    let budget = budget();
    let index = build_index(&documents, IndexLimits::default(), &budget);
    let modes = insensitive_modes().into_iter().chain([
        SearchMode::default(), SearchMode::RawBytes,
        SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Canonical },
    ]);
    for mode in modes {
        for raw in ["kelvin allow", "kelvin -blocked", "kelvin allow -blocked",
            "Kelvin allow", "Kelvin absent", "k allow", "banana absent", "abcd"] {
            let query = ParsedQuery::parse(raw).unwrap();
            for limit in [0, 1, 100] {
                let opts = options(mode).with_max_matches(limit);
                let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
                let report = index.search(&query, opts.clone(), &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected)
                    .unwrap_or_else(|error| panic!("{raw}, {mode:?}, limit {limit}: {error}"));
                drop(report);
                let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
                    &documents, &query, &opts, || false).unwrap();
                let report = index.search_counting(&query, opts, &budget, allocation(2), || false).unwrap();
                ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected)
                    .unwrap_or_else(|error| panic!("counting {raw}, {mode:?}, limit {limit}: {error}"));
            }
        }
    }
    let opts = options(insensitive_modes()[0]);
    // A short or Unicode primary does not disable an ASCII required-term proof.
    assert!(!index.may_match(1, &ParsedQuery::parse("k allow").unwrap(), &opts).unwrap());
    assert!(!index.may_match(1, &ParsedQuery::parse("Kelvin allow").unwrap(), &opts).unwrap());
    // Existing exact verification still rejects a false positive with all grams.
    assert!(index.may_match(0, &ParsedQuery::parse("abcd").unwrap(), &opts).unwrap());
    // Case-sensitive canonical queries continue using exact raw grams.
    let sensitive = options(SearchMode::DecodedText {
        case_sensitive: true, normalization: UnicodeNormalization::Canonical });
    assert!(!index.may_match(0, &ParsedQuery::parse("allow").unwrap(), &sensitive).unwrap());
    assert!(index.may_match(0, &ParsedQuery::parse("ALLOW").unwrap(), &sensitive).unwrap());
}

#[test]
fn normalized_candidates_reach_late_hits_with_only_the_matching_files_byte_budget() {
    let noise = b"common symbols without queried identifiers ".repeat(64);
    let target = b"RaRe_NeEdLe rare_needLE";
    let mut captures: Vec<_> = (1..=32).map(|id| capture(id, &noise)).collect();
    captures.push(capture(33, target));
    let documents: Vec<_> = captures.iter().map(document).collect();
    let budget = budget();
    let query = ParsedQuery::parse("rare_needle").unwrap();
    let index = build_index(&documents, IndexLimits::default(), &budget);
    let retained = budget.accounting().reserved().get();
    for limit in [0, 1, 2] {
        let opts = options(insensitive_modes()[0]).with_max_matches(limit);
        let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
            &documents, &query, &opts, || false).unwrap();
        let mut bounded = opts;
        bounded.max_bytes_scanned = Some(target.len() as u64);
        let report = index.search_counting(&query, bounded, &budget, allocation(2), || false).unwrap();
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.capture_results().matches.len(), limit);
        assert_eq!(report.skipped_by_index(), 32);
        assert_eq!(report.scan_attempts(), 1);
        assert_eq!(report.fallback_attempts(), 0);
        assert_eq!(report.capture_results().scanned_bytes, target.len() as u64);
        drop(report);
        assert_eq!(budget.accounting().reserved().get(), retained);
    }

    let owned = index.into_owned(SourceRetentionLimits::default(), &budget, allocation(3), || false).unwrap();
    let statistics = owned.statistics();
    for limit in [0, 1, 2] {
        let opts = options(insensitive_modes()[0]).with_max_matches(limit);
        let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
            &documents, &query, &opts, || false).unwrap();
        let mut bounded = opts;
        bounded.max_bytes_scanned = Some(target.len() as u64);
        let mut cursor = OwnedIndexedQuery::new_counting(&owned, query.clone(), bounded,
            &budget, [allocation(4), allocation(5)], || false).unwrap();
        let admitted = budget.accounting().reserved().get();
        for _ in 0..33 {
            cursor.step(&owned, 1, generation(), || false).unwrap();
            assert_eq!(budget.accounting().reserved().get(), admitted);
        }
        ReferenceScanOracle::verify_oracle_match(cursor.report().capture_results(), &expected).unwrap();
        assert!(cursor.report().is_complete());
        assert_eq!(cursor.report().skipped_by_index(), 32);
        assert_eq!(cursor.report().scan_attempts(), 1);
        assert_eq!(cursor.report().fallback_attempts(), 0);
        assert_eq!(owned.statistics(), statistics);
    }
    drop(owned);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if bom { bytes.extend_from_slice(if little { &[0xff, 0xfe] } else { &[0xfe, 0xff] }); }
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn unicode_terms_sources_and_incompatible_decoders_keep_exact_fallback() {
    let insensitive = insensitive_modes()[0];
    let folded = insensitive_modes()[1];
    let canonical = insensitive_modes()[3];
    let cases = [
        // Unicode query transforms to ASCII despite the source's ASCII proof.
        (b"KELVIN".to_vec(), "KELVIN", insensitive, None, Some(1), true),
        // A late Unicode scalar prevents any whole-capture ASCII certificate.
        (format!("{}Kelvin", "x".repeat(8192)).into_bytes(), "kelvin", insensitive, None, Some(1), true),
        (format!("NEEDLE {}é", "x".repeat(8192)).into_bytes(), "needle", insensitive, None, Some(1), true),
        (format!("{}é", "x".repeat(8192)).into_bytes(), "absent", insensitive, None, Some(0), true),
        ("Straße".as_bytes().to_vec(), "STRASSE", folded, None, Some(1), true),
        ("ßß".as_bytes().to_vec(), "sss", folded, None, None, true),
        ("ßß".as_bytes().to_vec(), "ss", folded, None, None, true),
        ("cafe\u{301}".as_bytes().to_vec(), "café", canonical, None, Some(1), true),
        // BOMs are part of captured bytes, not a reason to bless a raw ASCII prefix.
        (b"\xef\xbb\xbfNEEDLE".to_vec(), "needle", insensitive, None, Some(1), true),
        (utf16("NEEDLE", true, true), "needle", insensitive, None, Some(1), true),
        (utf16("NEEDLE", false, true), "needle", insensitive, None, Some(1), true),
        // BOMless ASCII UTF-16 byte pairs pass bytes.is_ascii(): the declared
        // decoder must independently prohibit raw UTF-8 gram certificates.
        (utf16("NEEDLE", true, false), "needle", insensitive, Some(DetectedEncoding::Utf16Le), Some(1), true),
        (utf16("NEEDLE", false, false), "needle", insensitive, Some(DetectedEncoding::Utf16Be), Some(1), true),
        (b"NEEDLE\xff".to_vec(), "absent", insensitive, None, None, false),
        (b"NEEDLE".to_vec(), "absent", insensitive, Some(DetectedEncoding::Unsupported), None, false),
    ];
    for (bytes, raw, mode, encoding, count, complete) in cases {
        let source = capture(1, &bytes);
        let documents = [document(&source)];
        let budget = budget();
        let index = build_index(&documents, IndexLimits::default(), &budget);
        let query = ParsedQuery::parse(raw).unwrap();
        let mut opts = options(mode);
        opts.declared_encoding = encoding;
        assert!(index.may_match(0, &query, &opts).unwrap(), "{raw}, {mode:?}, {encoding:?}");
        let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
        let report = index.search(&query, opts, &budget, allocation(2), || false).unwrap();
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        assert_eq!(report.is_complete(), complete);
        assert_eq!(report.skipped_by_index(), 0);
        assert_eq!(report.scan_attempts(), 1);
        assert_eq!(report.fallback_attempts(), 1);
        if let Some(count) = count { assert_eq!(report.capture_results().matches.len(), count); }
        if !complete {
            assert_eq!(report.capture_results().unsupported_files, [source.request().file()]);
        }
    }
}

#[test]
fn quota_uncovered_ascii_sources_never_become_negative_certificates() {
    let captures = [capture(1, b"BANANA ALLOW"), capture(2, b"BANANA WITHOUT PERMISSION")];
    let documents: Vec<_> = captures.iter().map(document).collect();
    let query = ParsedQuery::parse("ana allow").unwrap();
    let opts = options(insensitive_modes()[0]);
    let expected = ReferenceScanOracle::scan_collection(&documents, &query, &opts).unwrap();
    for limits in [
        IndexLimits { max_source_bytes_per_file: 1, ..Default::default() },
        IndexLimits { max_scratch_bytes: 0, ..Default::default() },
        IndexLimits { max_grams_per_file: 0, ..Default::default() },
        IndexLimits { max_total_grams: 0, ..Default::default() },
        IndexLimits { max_source_bytes_total: 0, ..Default::default() },
    ] {
        let budget = budget();
        let index = build_index(&documents, limits, &budget);
        assert_eq!(index.statistics().uncovered_files, 2);
        assert!(index.may_match(1, &query, &opts).unwrap());
        let report = index.search(&query, opts.clone(), &budget, allocation(2), || false).unwrap();
        ReferenceScanOracle::verify_oracle_match(report.capture_results(), &expected).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.skipped_by_index(), 0);
        assert_eq!(report.scan_attempts(), 2);
        assert_eq!(report.fallback_attempts(), 2);
        drop(report);
        drop(index);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn incremental_owned_builder_retains_each_captures_ascii_proof() {
    let captures = [capture(1, b"unrelated"), capture(2, b"RaRe_NeEdLe"),
        capture(3, "unrelated Kelvin".as_bytes())];
    let documents: Vec<_> = captures.iter().map(document).collect();
    let budget = budget();
    let mut builder = OwnedIndexBuilder::new(SearchManifestId::new(owner(), 1).unwrap(),
        SourceRetentionLimits { max_files: 3, max_source_bytes: 1024, max_path_bytes: 256 },
        IndexLimits::default(), &budget, allocation(1), allocation(2), || false).unwrap();
    for source in &captures {
        builder.push_capture(source, "source.rs", &budget, allocation(3), || false).unwrap();
    }
    let owned = builder.finish(MembershipState::Closed, || false).unwrap();
    let query = ParsedQuery::parse("rare_needle").unwrap();
    let opts = options(insensitive_modes()[1]);
    let expected = ReferenceScanOracle::scan_collection_counting_with_cancel(
        &documents, &query, &opts, || false).unwrap();
    let mut cursor = OwnedIndexedQuery::new_counting(&owned, query, opts,
        &budget, [allocation(3), allocation(4)], || false).unwrap();
    for _ in 0..3 { cursor.step(&owned, 1, generation(), || false).unwrap(); }
    ReferenceScanOracle::verify_oracle_match(cursor.report().capture_results(), &expected).unwrap();
    assert!(cursor.report().is_complete());
    assert_eq!(cursor.report().skipped_by_index(), 1);
    assert_eq!(cursor.report().scan_attempts(), 2);
    assert_eq!(cursor.report().fallback_attempts(), 1);
    drop(cursor);
    drop(owned);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

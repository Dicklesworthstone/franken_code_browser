use fcb_test_support::{
    ContentDigest, DefectClass, FixtureKind, GraphError, LayoutError, MinimizeError, Outcome,
    ReceiptError, ScanError, Corpus, reference_byte_scan, reference_layout, reference_line_scan,
    reference_topological_order, line_for_byte, minimize_failure,
};

#[test]
fn seeded_corpus_is_measured_and_repeatable() {
    let first = Corpus::seeded(41);
    let second = Corpus::seeded(41);
    let changed = Corpus::seeded(42);

    assert_eq!(first, second);
    assert_ne!(first.manifest().digest, changed.manifest().digest);
    assert_eq!(first.manifest().file_count, 13);
    assert!(first.manifest().total_bytes > 0);
    assert_eq!(first.manifest().generator_version, "fcb-corpus-1");
    assert!(first.files().iter().any(|file| file.kind() == FixtureKind::Source));
    assert!(first.files().iter().any(|file| file.kind() == FixtureKind::Path));
    assert!(first.files().iter().any(|file| file.kind() == FixtureKind::Tree));
    assert!(first.files().iter().any(|file| file.kind() == FixtureKind::Document));
    assert!(first.files().iter().any(|file| file.kind() == FixtureKind::Artifact));
    assert!(first.files().iter().all(|file| file.digest() == ContentDigest::of(file.bytes())));
}

#[test]
fn mixed_language_fixture_has_license_provenance_and_raw_paths() {
    let corpus = Corpus::default_seeded();
    let languages: Vec<_> = corpus.files().iter().map(|file| file.language()).collect();

    assert!(languages.contains(&"rust"));
    assert!(languages.contains(&"python"));
    assert!(languages.contains(&"javascript"));
    assert!(languages.contains(&"shell"));
    assert!(languages.contains(&"yaml"));
    assert!(corpus.file(b"LICENSE").is_some_and(|file| file.license() == "MIT"));
    assert!(corpus
        .file(b"PROVENANCE.md")
        .is_some_and(|file| file.provenance().contains("FCB-authored")));
    assert!(corpus.files().iter().any(|file| file.kind() == FixtureKind::Unicode));
    assert!(corpus
        .files()
        .iter()
        .any(|file| file.kind() == FixtureKind::Path && file.path().contains(&0xff)));
    assert!(corpus
        .files()
        .iter()
        .any(|file| file.kind() == FixtureKind::Unicode && file.bytes().windows(4).any(|window| window == "😀".as_bytes())));
}

#[test]
fn byte_and_line_oracles_preserve_exact_boundaries() {
    let bytes = b"aa\r\nbbb\ncc\rdd";
    assert_eq!(
        reference_byte_scan(bytes, b"a", 1024).unwrap(),
        vec![
            fcb_test_support::ByteMatch { start: 0, end: 1 },
            fcb_test_support::ByteMatch { start: 1, end: 2 },
        ]
    );
    let lines = reference_line_scan(bytes, 1024, 8).unwrap();
    assert_eq!(lines[0].start, 0);
    assert_eq!(lines[0].content_end, 2);
    assert_eq!(lines[0].end, 4);
    assert_eq!(lines[1].start, 4);
    assert_eq!(lines[1].content_end, 7);
    assert_eq!(lines[1].end, 8);
    assert_eq!(line_for_byte(&lines, 8), Ok(2));
    assert_eq!(line_for_byte(&lines, bytes.len() as u64), Err(ScanError::OffsetOutsideInput));
    assert_eq!(reference_byte_scan(bytes, b"", 1024), Err(ScanError::EmptyNeedle));
    assert_eq!(
        reference_line_scan(b"a\nb\nc", 1024, 2),
        Err(ScanError::TooManyLines)
    );
    assert_eq!(
        reference_line_scan(b"bounded", 3, 8),
        Err(ScanError::InputTooLarge)
    );
}

#[test]
fn graph_and_layout_oracles_are_checked_and_deterministic() {
    let expected = reference_topological_order(4, &[(0, 2), (1, 2), (2, 3)]).unwrap();
    assert_eq!(expected, vec![0, 1, 2, 3]);
    let intentionally_wrong = vec![0, 2, 1, 3];
    assert_ne!(intentionally_wrong, expected);
    assert_eq!(
        reference_topological_order(2, &[(0, 1), (1, 0)]),
        Err(GraphError::Cycle)
    );
    assert_eq!(
        reference_topological_order(2, &[(0, 1), (0, 1)]),
        Err(GraphError::DuplicateEdge)
    );

    let layout = reference_layout(&[1, 2, 1], 100, 20).unwrap();
    assert_eq!(layout[0].width, 25);
    assert_eq!(layout[1].width, 50);
    assert_eq!(layout[2].x, 75);
    assert_eq!(layout[2].width, 25);
    assert_eq!(reference_layout(&[0, 0], 100, 20), Err(LayoutError::ZeroTotalWeight));
}

#[test]
fn minimizer_is_bounded_and_preserves_defect_class() {
    let minimized = minimize_failure(
        b"prefix:bad-answer:suffix",
        DefectClass::WrongAnswer,
        100,
        |input| input.windows(3).any(|window| window == b"bad").then_some(DefectClass::WrongAnswer),
    )
    .unwrap();
    assert!(minimized.input.windows(3).any(|window| window == b"bad"));
    assert!(minimized.input.len() < b"prefix:bad-answer:suffix".len());
    assert!(minimized.attempts <= 100);

    assert_eq!(
        minimize_failure(b"x", DefectClass::WrongAnswer, 10, |_| Some(DefectClass::CorruptInput)),
        Err(MinimizeError::InitialDefectMismatch)
    );
    assert_eq!(
        minimize_failure(b"x", DefectClass::WrongAnswer, 0, |_| Some(DefectClass::WrongAnswer)),
        Err(MinimizeError::BudgetExhausted)
    );
    assert_eq!(
        minimize_failure(
            &vec![b'x'; 65 * 1024],
            DefectClass::WrongAnswer,
            10,
            |_| Some(DefectClass::WrongAnswer),
        ),
        Err(MinimizeError::InputTooLarge)
    );
}

#[test]
fn receipts_cover_outcomes_and_redaction_without_losing_measurements() {
    let corpus = Corpus::seeded(99);
    for outcome in [Outcome::Success, Outcome::DeliberateFailure, Outcome::Interrupted] {
        let receipt = corpus.receipt(outcome, 3).unwrap();
        assert!(receipt.is_truthful_for(&corpus));
        assert_eq!(receipt.redacted().outcome, outcome);
        assert!(receipt.redacted().redacted);
    }
    assert_eq!(
        corpus.receipt(Outcome::Success, 1_000_001),
        Err(ReceiptError::AttemptsTooLarge)
    );
}

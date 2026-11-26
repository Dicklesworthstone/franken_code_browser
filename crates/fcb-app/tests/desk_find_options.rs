#![forbid(unsafe_code)]

mod support;
use support::{Json, parse};
use fcb::{ArenaOwnerId, FileId, SourceCapture, SourceRevision};
use fcb::search::{QueryError, ResourceAllocationId};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskCommand, DeskError, DeskFindMode,
    DeskFindOptions, DeskLimits, DeskPaneId, DeskSession, DeskSessionError}};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2817).unwrap() }
fn session() -> DeskSession { DeskSession::new(owner(), DeskLimits::default()).unwrap() }
fn adopt(s: &mut DeskSession, bytes: &[u8]) -> DeskPaneId {
    let source = SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap(), "unavailable/source.rs", bytes.to_vec()).unwrap();
    s.adopt(0, 1, source, 0, None, || false).unwrap().active.unwrap()
}
fn json(response: &HostResponse) -> Json {
    let value = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(value.get("schema").text(), "fcb.reading-desk/1");
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
    value
}
fn options(mode: DeskFindMode, count_all_matches: bool, start_after: u64) -> DeskFindOptions {
    DeskFindOptions { mode, count_all_matches, start_after, ..Default::default() }
}
fn encode(text: &str, encoding: usize) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
        _ => {
            let mut bytes = if encoding == 2 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding == 2 { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            bytes
        }
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(hit: &Json) -> (u64, u64) {
    let range = hit.get("original_range");
    (range.get("start").number(), range.get("end").number())
}

#[test]
fn counted_windows_preserve_overlaps_original_encodings_and_page_local_activation() {
    let text = "bananana BANANANA\r\nana";
    // Independent byte oracle for this ASCII case-insensitive fixture.
    let expected: Vec<_> = text.as_bytes().windows(3).enumerate()
        .filter_map(|(index, bytes)| bytes.eq_ignore_ascii_case(b"ana").then_some(index)).collect();
    assert_eq!(expected, [1, 3, 5, 10, 12, 14, 19]);
    for encoding in 0..4 {
        let bytes = encode(text, encoding);
        let mut s = session(); let pane = adopt(&mut s, &bytes);
        let options = DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Literal, true, 2) };
        let response = s.search_with_options(1, pane, 1, "ana", 2, bytes.len() as u64, options, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let page = json(&response);
        assert!(page.get("search_complete").flag());
        assert!(page.get("match_count_complete").flag());
        assert!(page.get("rows_truncated").flag());
        assert!(page.get("has_more_matches").flag());
        assert_eq!(page.get("matches_seen").number(), expected.len() as u64);
        assert_eq!(page.get("skipped_matches").number(), 2);
        assert_eq!(page.get("retained_hits").number(), 2);
        for (index, hit) in page.get("hits").array().iter().enumerate() {
            let start = expected[index + 2];
            assert_eq!(hit.get("hit").number(), index as u64);
            assert_eq!(hit.get("occurrence_id").number(), index as u64 + 3);
            assert_eq!(bounds(hit), (encode(&text[..start], encoding).len() as u64,
                encode(&text[..start + 3], encoding).len() as u64));
        }
        assert_eq!(s.model().revision(), 1);
        s.activate_hit(1, 2, pane, 1, 1, || false).unwrap();
        let (start, end) = bounds(&page.get("hits").array()[1]);
        assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(),
            hex(&bytes[start as usize..end as usize]));

        let response = s.search_with_options(2, pane, 2, "ana", 2, bytes.len() as u64,
            DeskFindOptions { start_after: 6, ..options }, || false).unwrap();
        let last = json(&response);
        assert!(last.get("search_complete").flag());
        assert!(!last.get("has_more_matches").flag());
        assert_eq!(last.get("hits").array().len(), 1);
        assert_eq!(last.get("hits").array()[0].get("occurrence_id").number(), 7);
        assert!(matches!(s.activate_hit(2, 3, pane, 1, 0, || false), Err(DeskSessionError::StaleQuery)));
        s.activate_hit(2, 3, pane, 2, 0, || false).unwrap();
    }
}

#[test]
fn unicode_lowercase_preserves_expansion_ranges_and_distinct_occurrence_ids() {
    for encoding in 0..4 {
        let bytes = encode("İi\u{307}İ KkK", encoding);
        let mut s = session(); let pane = adopt(&mut s, &bytes);
        let response = s.search_with_options(1, pane, 1, "i", 8, bytes.len() as u64,
            DeskFindOptions { case_sensitive: false, ..Default::default() }, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let page = json(&response);
        assert_eq!(page.get("matches_seen").number(), 3);
        let prefixes = [("", "İ"), ("İ", "İi"), ("İi\u{307}", "İi\u{307}İ")];
        for (index, ((start, end), hit)) in prefixes.iter().zip(page.get("hits").array()).enumerate() {
            assert_eq!(bounds(hit), (encode(start, encoding).len() as u64, encode(end, encoding).len() as u64));
            assert_eq!(hit.get("occurrence_id").number(), index as u64 + 1);
        }
        let response = s.search_with_options(1, pane, 2, "k", 0, bytes.len() as u64,
            DeskFindOptions { case_sensitive: false, count_all_matches: true, ..Default::default() }, || false).unwrap();
        let counted = json(&response);
        assert!(counted.get("match_count_complete").flag());
        assert_eq!(counted.get("matches_seen").number(), 3);
        assert!(counted.get("hits").array().is_empty());
    }
}

#[test]
fn count_only_empty_and_beyond_end_windows_report_coverage_separately_from_rows() {
    let mut s = session(); let pane = adopt(&mut s, b"aaaaa");
    let response = s.search_with_options(1, pane, 1, "aa", 0, 5,
        options(DeskFindMode::Literal, true, 0), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let count = json(&response);
    assert!(count.get("search_complete").flag()); assert!(count.get("rows_truncated").flag());
    assert!(count.get("has_more_matches").flag()); assert_eq!(count.get("matches_seen").number(), 4);
    assert!(count.get("hits").array().is_empty());
    assert!(matches!(s.activate_hit(1, 2, pane, 1, 0, || false), Err(DeskSessionError::MissingHit)));

    let response = s.search_with_options(1, pane, 2, "aa", 2, 5,
        options(DeskFindMode::Literal, true, u64::MAX), || false).unwrap();
    let end = json(&response);
    assert_eq!(end.get("page_start_after").number(), u64::MAX);
    assert_eq!(end.get("skipped_matches").number(), 4);
    assert!(end.get("search_complete").flag()); assert!(!end.get("has_more_matches").flag());
    assert!(end.get("rows_truncated").flag()); assert!(end.get("hits").array().is_empty());

    let response = s.search_with_options(1, pane, 3, "aa", 1, 2,
        options(DeskFindMode::Literal, false, 3), || false).unwrap();
    let partial = json(&response);
    assert!(!partial.get("search_complete").flag());
    assert!(!partial.get("match_count_complete").flag());
    assert!(!partial.get("has_more_matches").flag());
    assert!(partial.get("hits").array().is_empty());
    assert!(partial.get("scanned_bytes").number() <= 2);

    let response = s.search_with_options(1, pane, 4, "missing", 0, 5,
        options(DeskFindMode::Literal, true, 0), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    assert_eq!(json(&response).get("matches_seen").number(), 0);
}

#[test]
fn expressions_wait_for_late_required_and_excluded_terms_before_exposing_hits() {
    let text = format!("Anchor anchor{}required phrase", " ".repeat(70_000));
    let mut s = session(); let pane = adopt(&mut s, text.as_bytes());
    let options = DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Expression, false, 0) };
    let response = s.search_with_options(1, pane, 1, "anchor \"required phrase\" -forbidden",
        1, text.len() as u64, options, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let accepted = json(&response);
    assert!(accepted.get("expression_qualified").flag());
    assert_eq!(accepted.get("predicate_scope").text(), "retained-source-capture");
    assert!(accepted.get("match_count_complete").flag());
    assert_eq!(accepted.get("matches_seen").number(), 2);
    assert_eq!(bounds(&accepted.get("hits").array()[0]), (0, 6));

    let response = s.search_with_options(1, pane, 2, "anchor \"required phrase\" -forbidden",
        1, 12, options, || false).unwrap();
    let partial = json(&response);
    assert_eq!(partial.get("expression_qualified"), &Json::Null);
    assert!(!partial.get("search_complete").flag());
    assert!(partial.get("hits").array().is_empty()); assert_eq!(partial.get("matches_seen").number(), 0);

    let response = s.search_with_options(1, pane, 3, "anchor -\"required phrase\"",
        1, text.len() as u64, options, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let excluded = json(&response);
    assert!(excluded.get("search_complete").flag());
    assert!(!excluded.get("expression_qualified").flag());
    assert!(excluded.get("hits").array().is_empty()); assert_eq!(excluded.get("matches_seen").number(), 0);
}

#[test]
fn malformed_and_unqualified_queries_keep_the_old_accepted_generation_usable() {
    let mut s = session(); let pane = adopt(&mut s, b"original other");
    s.search(1, pane, 1, "original", 8, 100, || false).unwrap();
    let expression = options(DeskFindMode::Expression, false, 0);
    for (generation, query) in [(2, "other path:source.rs"), (3, "other type:rust"), (4, "other -lang:rust")] {
        assert!(matches!(s.search_with_options(1, pane, generation, query, 8, 100, expression, || false),
            Err(DeskSessionError::ExpressionScopeUnsupported)));
        assert_eq!(s.accepted_query(), Some(1));
    }
    assert!(matches!(s.search_with_options(1, pane, 5, "\"unclosed", 8, 100, expression, || false),
        Err(DeskSessionError::Query(QueryError::SyntaxError))));
    assert!(matches!(s.search_with_options(1, pane, 6, "regex:other", 8, 100, expression, || false),
        Err(DeskSessionError::Query(QueryError::RegexUnqualified))));
    assert!(matches!(s.search_with_options(1, pane, 6, "other", 8, 100, expression, || false),
        Err(DeskSessionError::StaleQuery)));
    assert!(matches!(s.search_with_options(0, pane, 7, "other", 8, 100, expression, || false),
        Err(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert_eq!(s.accepted_query(), Some(1));
    s.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(), hex(b"original"));
    assert!(DeskSessionError::Query(QueryError::Canceled).is_canceled());
}

#[test]
fn identifiers_cross_scan_boundaries_and_include_textual_comments_and_strings() {
    let prefix = fcb::search::streaming::STREAM_BUFFER_BYTES - 1;
    let mut bytes = vec![b' '; prefix];
    bytes.extend_from_slice(b"foo foobar foo_ $foo foo foo\n// foo\n\"foo\"");
    let mut s = session(); let pane = adopt(&mut s, &bytes);
    let response = s.search_with_options(1, pane, 1, "foo", 8, bytes.len() as u64,
        options(DeskFindMode::Identifier, true, 0), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let page = json(&response);
    assert_eq!(page.get("mode").text(), "whole-token-text-candidate");
    let expected = [0, 21, 25, 32, 37];
    assert_eq!(page.get("hits").array().len(), expected.len());
    for (hit, offset) in page.get("hits").array().iter().zip(expected) {
        assert_eq!(bounds(hit), ((prefix + offset) as u64, (prefix + offset + 3) as u64));
    }
    let lowercase = s.search_with_options(1, pane, 2, "FOO", 8, bytes.len() as u64,
        DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Identifier, false, 0) },
        || false).unwrap();
    assert_eq!(json(&lowercase).get("matches_seen").number(), expected.len() as u64);
    assert!(!json(&lowercase).get("case_sensitive").flag());
    assert!(matches!(s.search_with_options(1, pane, 3, "foo bar", 8, bytes.len() as u64,
        DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Identifier, false, 0) },
        || false), Err(DeskSessionError::Stream(_))));
    assert_eq!(s.accepted_query(), Some(2));
}

#[test]
fn cancellation_during_replacement_preserves_old_rows_selection_and_personal_notes() {
    let mut bytes = b"original ".to_vec(); bytes.extend(vec![b'x'; 200_000]);
    let mut s = session(); let pane = adopt(&mut s, &bytes);
    s.apply(1, 2, DeskCommand::Annotate { pane, label: "why".into(), body: "keep this context".into() }, || false).unwrap();
    s.search(2, pane, 1, "original", 8, bytes.len() as u64, || false).unwrap();
    let before = s.model().location(pane, 2).unwrap();
    let mut polls = 0;
    let result = s.search_with_options(2, pane, 2, "x", 2, bytes.len() as u64,
        options(DeskFindMode::Literal, true, 0), || { polls += 1; polls == 3 });
    assert!(matches!(result, Err(error) if error.is_canceled()));
    assert!(polls >= 3);
    assert_eq!(s.accepted_query(), Some(1));
    assert_eq!(s.model().location(pane, 2).unwrap(), before);
    assert_eq!(s.model().bookmarks()[0].body(), "keep this context");
    s.activate_hit(2, 3, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(3, pane, || false).unwrap()).get("original_hex").text(), hex(b"original"));
}

#[test]
fn restored_captures_support_the_same_modes_without_live_paths_or_losing_notes() {
    let mut first = session(); let pane = adopt(&mut first, b"Anchor anchor required");
    first.apply(1, 2, DeskCommand::Annotate { pane, label: "offline".into(), body: "saved explanation".into() }, || false).unwrap();
    let checkpoint = first.model().checkpoint(2, ResourceAllocationId::new(90_001).unwrap(), || false).unwrap();
    let mut restored = session();
    let pane = restored.restore_checkpoint_bytes(0, 1, checkpoint.bytes(), || false).unwrap().active.unwrap();
    let response = restored.search_with_options(1, pane, 1, "anchor required -missing", 1, 100,
        DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Expression, true, 1) }, || false).unwrap();
    let found = json(&response);
    assert_eq!(found.get("matches_seen").number(), 2);
    assert_eq!(found.get("hits").array()[0].get("occurrence_id").number(), 2);
    assert_eq!(bounds(&found.get("hits").array()[0]), (7, 13));
    restored.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&restored.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(), hex(b"anchor"));
    assert_eq!(restored.model().bookmarks()[0].body(), "saved explanation");
    assert_eq!(json(&restored.state(|| false).unwrap()).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn lowercase_identifier_windows_keep_whole_tokens_original_encodings_and_activation() {
    let prefix = " ".repeat(fcb::search::streaming::STREAM_BUFFER_BYTES - 5);
    let text = format!("{prefix}k K K kx _K 1K\nİ i\u{307} I\u{307} i I ß ẞ ss SS ΟΣ οσ ος É é e\u{301}");
    for encoding in 0..4 {
        let bytes = encode(&text, encoding);
        let mut s = session(); let pane = adopt(&mut s, &bytes);
        let lower = DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Identifier, true, 1) };
        let response = s.search_with_options(1, pane, 1, "K", 2, bytes.len() as u64, lower, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let page = json(&response);
        assert_eq!(page.get("mode").text(), "whole-token-text-candidate");
        assert!(!page.get("case_sensitive").flag());
        assert!(page.get("search_complete").flag()); assert!(page.get("match_count_complete").flag());
        assert_eq!(page.get("matches_seen").number(), 3);
        assert_eq!(page.get("skipped_matches").number(), 1);
        assert_eq!(page.get("hits").array().len(), 2);
        assert!(!page.get("has_more_matches").flag());
        let kelvin = &page.get("hits").array()[1];
        assert_eq!(kelvin.get("hit").number(), 1); assert_eq!(kelvin.get("occurrence_id").number(), 3);
        let start = text.find('K').unwrap();
        assert_eq!(bounds(kelvin), (encode(&text[..start], encoding).len() as u64,
            encode(&text[..start + 'K'.len_utf8()], encoding).len() as u64));
        s.activate_hit(1, 2, pane, 1, 1, || false).unwrap();
        let (start, end) = bounds(kelvin);
        assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(),
            hex(&bytes[start as usize..end as usize]));

        for (generation, query, expected) in [(2, "i", vec!["i", "I"]),
            (3, "i\u{307}", vec!["İ", "i\u{307}", "I\u{307}"]),
            (4, "ß", vec!["ß", "ẞ"]), (5, "ss", vec!["ss", "SS"]),
            (6, "οσ", vec!["ΟΣ", "οσ"]), (7, "ος", vec!["ος"]),
            (8, "é", vec!["É", "é"])] {
            let response = s.search_with_options(2, pane, generation, query, 16, bytes.len() as u64,
                DeskFindOptions { start_after: 0, ..lower }, || false).unwrap();
            assert_eq!(response.exit_code(), EXIT_OK);
            let report = json(&response);
            assert_eq!(report.get("matches_seen").number(), expected.len() as u64);
            for (hit, spelling) in report.get("hits").array().iter().zip(expected) {
                let (start, end) = bounds(hit);
                let original = encode(spelling, encoding);
                assert_eq!(&bytes[start as usize..end as usize], &original[encode("", encoding).len()..]);
            }
        }
        let beyond = json(&s.search_with_options(2, pane, 9, "K", 1, bytes.len() as u64,
            DeskFindOptions { start_after: u64::MAX, ..lower }, || false).unwrap());
        assert_eq!(beyond.get("matches_seen").number(), 3);
        assert_eq!(beyond.get("skipped_matches").number(), 3);
        assert!(beyond.get("hits").array().is_empty()); assert!(beyond.get("match_count_complete").flag());
    }
}

#[test]
fn lowercase_identifier_witness_can_exceed_query_limit_and_survive_failed_replacement() {
    let name = "K".repeat(1024); let spelling = "K".repeat(1024);
    let text = format!("{spelling} {name}");
    let mut s = session(); let pane = adopt(&mut s, text.as_bytes());
    let lower = DeskFindOptions { case_sensitive: false, ..options(DeskFindMode::Identifier, true, 0) };
    let page = json(&s.search_with_options(1, pane, 1, &name, 1, text.len() as u64, lower, || false).unwrap());
    assert_eq!(page.get("matches_seen").number(), 2);
    assert_eq!(bounds(&page.get("hits").array()[0]), (0, spelling.len() as u64));
    assert!(s.search_with_options(1, pane, 2, &name, 1, text.len() as u64,
        DeskFindOptions { case_sensitive: true, ..lower }, || true).err().unwrap().is_canceled());
    assert_eq!(s.accepted_query(), Some(1));
    s.activate_hit(1, 2, pane, 1, 0, || false).unwrap();
    assert_eq!(json(&s.copy_selection(2, pane, || false).unwrap()).get("original_hex").text(),
        hex(spelling.as_bytes()));
    let count = json(&s.search_with_options(2, pane, 3, &name, 0, text.len() as u64, lower, || false).unwrap());
    assert_eq!(count.get("matches_seen").number(), 2);
    assert!(count.get("hits").array().is_empty()); assert!(count.get("match_count_complete").flag());
}

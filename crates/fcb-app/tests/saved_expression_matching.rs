#![forbid(unsafe_code)]
#![cfg(unix)]

#[path = "support/saved_desk.rs"]
mod fixture;
mod support;
use fcb_app::host::HostResponse;
use fixture::{Fixture, entry, owner};
use support::{Json, parse};

fn json(response: &HostResponse) -> Json {
    parse(response.as_str().as_bytes()).unwrap()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little {
        vec![0xff, 0xfe]
    } else {
        vec![0xfe, 0xff]
    };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        });
    }
    bytes
}

#[test]
fn lowercase_predicates_preserve_paging_index_and_utf16_reader_witnesses() {
    let little = utf16("K REQUIRED", true);
    let big = utf16("K REQUIRED", false);
    let f = Fixture::new(
        &[
            entry(b"a.rs", "K REQUIRED".as_bytes()),
            entry(b"b.rs", &little),
            entry(b"c.rs", &big),
            entry(b"d.rs", b"K REQUIRED FoRbIdDeN"),
        ],
        true,
    );
    let (path, pin) = f.index();
    let mut saved = f.open(66501);
    saved.attach_index(1, &path, pin, || false).unwrap();
    saved.search(1, "REQUIRED", 10, || false).unwrap();
    let before = json(&saved.info(|| false).unwrap())
        .get("index_page_loads")
        .number();
    let page = json(
        &saved
            .expression_search_matching(1, "k required -forbidden", 10, 10000, false, true, || {
                false
            })
            .unwrap(),
    );
    assert_eq!(page.get("mode").text(), "unicode-lowercase-expression");
    assert!(!page.get("case_sensitive").flag());
    assert_eq!(page.get("normalization").text(), "exact");
    assert_eq!(page.get("retained_hits").number(), 3);
    assert!(page.get("matches_seen_exact").flag());
    assert!(!page.get("index_used").flag());
    assert_eq!(saved.accepted_generation(), Some(1));
    assert_eq!(saved.index_generation(), Some(1));
    assert_eq!(
        json(&saved.info(|| false).unwrap())
            .get("index_page_loads")
            .number(),
        before
    );
    let reads = json(&saved.info(|| false).unwrap())
        .get("member_read_calls")
        .number();
    let second = json(&saved.expression_results(1, 1, 1, || false).unwrap());
    assert!(!second.get("case_sensitive").flag());
    assert_eq!(second.get("hits").array()[0].get("hit_id").number(), 2);
    assert_eq!(
        json(&saved.info(|| false).unwrap())
            .get("member_read_calls")
            .number(),
        reads
    );
    for (id, start, end, expected) in [(1, 0, 3, "e284aa"), (2, 2, 4, "2a21"), (3, 2, 4, "212a")] {
        let (mut reader, response) = saved
            .open_expression_hit_reader(owner(66502 + id), 1, id, || false)
            .unwrap();
        let opened = json(&response);
        let selection = opened.get("selection");
        assert!(!opened.get("case_sensitive").flag());
        assert_eq!(opened.get("mode").text(), "unicode-lowercase-expression");
        assert_eq!(opened.get("primary_needle").text(), "k");
        assert_eq!(selection.get("text").text(), "K");
        assert!(!selection.get("case_sensitive").flag());
        assert_eq!(selection.get("mode").text(), "unicode-lowercase-expression");
        assert_eq!(selection.get("original_range").get("start").number(), start);
        assert_eq!(selection.get("original_range").get("end").number(), end);
        assert_eq!(selection.get("original_hex").text(), expected);
        assert!(!opened.get("live_source_reopened").flag());
        assert_eq!(
            json(&reader.copy_range(start, end, || false).unwrap())
                .get("original_hex")
                .text(),
            expected
        );
    }
    assert_eq!(
        json(&saved.results(1, 0, 10, || false).unwrap())
            .get("retained_hits")
            .number(),
        4
    );
}

#[test]
fn matching_policy_is_frozen_across_failed_replacements_and_independent_readers() {
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA REQUIRED")], true);
    let mut saved = f.open(66601);
    let first = json(
        &saved
            .expression_search_matching(1, "ana required", 10, 10000, false, true, || false)
            .unwrap(),
    );
    assert_eq!(first.get("matches_seen").number(), 2);
    let (mut reader, receipt) = saved
        .open_expression_hit_reader(owner(66602), 1, 1, || false)
        .unwrap();
    assert_eq!(json(&receipt).get("selection").get("text").text(), "ANA");
    assert!(
        saved
            .expression_search_matching(2, "regex:ana", 10, 10000, true, false, || false)
            .is_err()
    );
    assert!(
        matches!(saved.expression_search_matching(3, "ana required", 10, 10000, true, false, || true),
        Err(error) if error.is_canceled())
    );
    let kept = json(&saved.expression_results(1, 0, 10, || false).unwrap());
    for field in [
        "expression_generation",
        "expression",
        "primary_needle",
        "mode",
        "case_sensitive",
        "normalization",
        "count_all_matches",
        "matches_seen",
        "hits",
    ] {
        assert_eq!(kept.get(field), first.get(field), "retained {field}");
    }
    assert_eq!(saved.last_expression_attempt(), 3);
    let exact = json(
        &saved
            .expression_search_matching(4, "ana required", 10, 10000, true, false, || false)
            .unwrap(),
    );
    assert!(exact.get("case_sensitive").flag());
    assert_eq!(exact.get("mode").text(), "decoded-text-expression");
    assert_eq!(exact.get("matches_seen").number(), 0);
    assert!(saved.expression_results(1, 0, 10, || false).is_err());
    drop(saved);
    assert_eq!(
        json(&reader.copy_range(1, 4, || false).unwrap())
            .get("original_hex")
            .text(),
        "414e41"
    );
}

#[test]
fn expansion_spans_and_count_only_work_limits_keep_original_source_semantics() {
    let f = Fixture::new(&[entry(b"a.rs", "İİ REQUIRED".as_bytes())], true);
    let mut saved = f.open(66701);
    let result = json(
        &saved
            .expression_search_matching(1, "i required", 2, 10000, false, true, || false)
            .unwrap(),
    );
    assert_eq!(result.get("matches_seen").number(), 2);
    let (_, opened) = saved
        .open_expression_hit_reader(owner(66702), 1, 1, || false)
        .unwrap();
    assert_eq!(json(&opened).get("selection").get("text").text(), "İ");
    assert_eq!(
        json(&opened).get("selection").get("original_hex").text(),
        "c4b0"
    );
    let complete = json(
        &saved
            .expression_search_matching(2, "i required", 0, 10000, false, true, || false)
            .unwrap(),
    );
    assert_eq!(complete.get("matches_seen").number(), 2);
    assert!(complete.get("matches_seen_exact").flag());
    assert!(complete.get("hits").array().is_empty());
    let partial = json(
        &saved
            .expression_search_matching(3, "i -forbidden", 0, 3, false, true, || false)
            .unwrap(),
    );
    assert_eq!(partial.get("state").text(), "work-limit");
    assert!(!partial.get("matches_seen_exact").flag());
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert!(partial.get("scanned_bytes").number() <= 3);
}

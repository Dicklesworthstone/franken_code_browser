#![forbid(unsafe_code)]
#![cfg(unix)]

#[path = "support/saved_desk.rs"]
mod fixture;
mod support;

use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
};

use fcb_app::host::HostResponse;
use fixture::{Fixture, entry, owner};
use support::{Json, parse};

const SOURCE_BUFFER_LIMIT: u64 = 64 * 1024;

fn json(response: &HostResponse) -> Json {
    parse(response.as_str().as_bytes()).unwrap()
}

fn source(prefix: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    bytes.resize(512 * 1024, b'x');
    bytes.extend_from_slice(suffix);
    bytes
}

fn damage_tail(fixture: &Fixture, source: &[u8]) {
    let start = fixture
        .bytes
        .windows(source.len())
        .position(|bytes| bytes == source)
        .unwrap();
    let mut file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    file.seek(SeekFrom::Start((start + source.len() - 1) as u64))
        .unwrap();
    file.write_all(&[source[source.len() - 1] ^ 1]).unwrap();
    file.flush().unwrap();
}

fn assert_bounded_source(page: &Json, length: usize) {
    assert_eq!(page.get("member_bytes_read").number(), length as u64);
    assert!(page.get("member_read_calls").number() > 1);
    assert!(page.get("peak_source_bytes").number() > 0);
    assert!(page.get("peak_source_bytes").number() <= SOURCE_BUFFER_LIMIT);
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
}

#[test]
fn large_saved_literal_counts_use_bounded_buffers_and_keep_exact_reader_witnesses() {
    let bytes = source(b"needle\n", b"\nneedle");
    let fixture = Fixture::new(&[entry(b"large.rs", &bytes)], true);
    let mut saved = fixture.open(77101);
    let page = json(&saved.search_counted(1, "needle", 1, || false).unwrap());

    assert!(page.get("search_complete").flag());
    assert!(page.get("matches_seen_exact").flag());
    assert_eq!(page.get("matches_seen").number(), 2);
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(page.get("scanned_bytes").number(), bytes.len() as u64);
    assert_bounded_source(&page, bytes.len());

    let before = json(&saved.info(|| false).unwrap());
    let replay = json(&saved.results(1, 0, 1, || false).unwrap());
    assert_eq!(replay.get("hits"), page.get("hits"));
    let after = json(&saved.info(|| false).unwrap());
    assert_eq!(
        after.get("member_bytes_read"),
        before.get("member_bytes_read")
    );
    assert_eq!(
        after.get("member_read_calls"),
        before.get("member_read_calls")
    );

    let (mut reader, opened) = saved.open_hit_reader(owner(77102), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
    assert!(!json(&opened).get("live_source_reopened").flag());
    drop(saved);
    assert_eq!(
        json(&reader.copy_range(0, 6, || false).unwrap())
            .get("original_hex")
            .text(),
        "6e6565646c65"
    );
}

#[test]
fn early_literal_limit_still_rejects_corrupt_unread_tail_without_replacing_results() {
    let bytes = source(b"old needle needle\n", b"\nunchanged tail");
    let fixture = Fixture::new(&[entry(b"large.rs", &bytes)], true);
    let mut saved = fixture.open(77201);
    let accepted = json(&saved.search(1, "old", 1, || false).unwrap());
    let (reader, _) = saved.open_hit_reader(owner(77202), 1, 1, || false).unwrap();

    damage_tail(&fixture, &bytes);
    // The second needle supplies lookahead within the first buffer. Its unread
    // suffix must still be verified before this candidate can replace "old".
    assert!(saved.search(2, "needle", 1, || false).is_err());
    assert_eq!(saved.accepted_generation(), Some(1));
    let replay = json(&saved.results(1, 0, 1, || false).unwrap());
    assert_eq!(replay.get("hits"), accepted.get("hits"));
    assert_eq!(replay.get("needle").text(), "old");
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn whole_member_expression_predicates_share_one_bounded_source_pass() {
    let bytes = source(b"needle\n", b"\nrequired");
    let fixture = Fixture::new(&[entry(b"large.rs", &bytes)], true);
    let mut saved = fixture.open(77301);
    let page = json(
        &saved
            .expression_search(
                1,
                "needle required -forbidden",
                10,
                bytes.len() as u64,
                || false,
            )
            .unwrap(),
    );

    assert!(page.get("complete").flag());
    assert!(page.get("matches_seen_exact").flag());
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(page.get("members_loaded").number(), 1);
    assert_eq!(page.get("loaded_bytes").number(), bytes.len() as u64);
    assert_eq!(page.get("scanned_bytes").number(), bytes.len() as u64);
    assert_bounded_source(&page, bytes.len());

    let reads = json(&saved.info(|| false).unwrap())
        .get("member_read_calls")
        .number();
    let replay = json(&saved.expression_results(1, 0, 1, || false).unwrap());
    assert_eq!(replay.get("hits"), page.get("hits"));
    assert_eq!(
        json(&saved.info(|| false).unwrap())
            .get("member_read_calls")
            .number(),
        reads
    );
    let (reader, opened) = saved
        .open_expression_hit_reader(owner(77302), 1, 1, || false)
        .unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
    assert_eq!(
        json(&opened).get("selection").get("original_hex").text(),
        "6e6565646c65"
    );
}

#[test]
fn expression_work_limit_drains_integrity_without_qualifying_unseen_predicates() {
    let bytes = source(b"needle\n", b"\nrequired");
    let fixture = Fixture::new(&[entry(b"large.rs", &bytes)], true);
    let mut saved = fixture.open(77401);
    let page = json(
        &saved
            .expression_search(1, "needle required -forbidden", 10, 4096, || false)
            .unwrap(),
    );

    assert_eq!(page.get("state").text(), "work-limit");
    assert!(!page.get("complete").flag());
    assert!(!page.get("matches_seen_exact").flag());
    assert_eq!(page.get("retained_hits").number(), 0);
    assert_eq!(page.get("matches_seen").number(), 0);
    assert!(page.get("scanned_bytes").number() <= 4096);
    assert_eq!(page.get("members_loaded").number(), 1);
    assert_eq!(page.get("loaded_bytes").number(), bytes.len() as u64);
    assert_bounded_source(&page, bytes.len());

    damage_tail(&fixture, &bytes);
    assert!(
        saved
            .expression_search(2, "needle required -forbidden", 10, 4096, || false)
            .is_err()
    );
    assert_eq!(saved.accepted_expression_generation(), Some(1));
    let replay = json(&saved.expression_results(1, 0, 10, || false).unwrap());
    assert_eq!(replay.get("state").text(), "work-limit");
    assert_eq!(replay.get("hits"), page.get("hits"));
}

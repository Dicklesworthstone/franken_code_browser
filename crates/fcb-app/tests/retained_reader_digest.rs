#![forbid(unsafe_code)]
#![cfg(unix)]

//! Capture witnesses bind original bytes, survive live source changes, and
//! remain available from a shared reader after its originating reader closes.

mod support;

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, store::Sha256};
use fcb_app::{EXIT_OK, host::{HostResponse, reader::{ReaderSession, ReaderSessionError}}};
use support::{Json, parse};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn supplied(value: u64, bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(value), Path::new("retained.bin"), bytes, || false).unwrap()
}
fn json(response: HostResponse) -> Json {
    assert_eq!(response.exit_code(), EXIT_OK);
    parse(response.as_str().as_bytes()).unwrap()
}
fn info(reader: &mut ReaderSession) -> Json { json(reader.info(|| false).unwrap()) }
fn digest(document: &Json) -> &str {
    let value = document.get("capture_sha256").text();
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    value
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-reader-digest-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn empty_and_abc_info_match_known_sha256_vectors() {
    let cases: &[(&[u8], &str)] = &[(b"", EMPTY_SHA256), (b"abc", ABC_SHA256)];
    for (index, &(bytes, expected)) in cases.iter().enumerate() {
        let mut reader = supplied(9501 + index as u64, bytes);
        for _ in 0..2 {
            let document = info(&mut reader);
            assert_eq!(document.get("schema").text(), "fcb.reader-session/1");
            assert_eq!(document.get("command").text(), "info");
            assert_eq!(digest(&document), expected);
            assert_eq!(document.get("captured_bytes").number(), bytes.len() as u64);
            assert_eq!(document.get("capture_origin").text(), "host-supplied");
            assert_eq!(document.get("initial_source_bytes_read").number(), 0);
            assert_eq!(document.get("initial_read_calls").number(), 0);
            assert_eq!(document.get("additional_source_bytes_read").number(), 0);
        }
    }
}

#[test]
fn malformed_and_utf16_witnesses_hash_original_bytes_before_decoding() {
    let cases: &[(&[u8], &str)] = &[
        (b"\xff\0a", "utf8"),
        (b"\xfe\0a", "utf8"),
        (b"\xff\xfea\0b\0\0\xd8", "utf16le"),
        (b"\xfe\xff\0a\0b\xd8\0", "utf16be"),
    ];
    let mut witnesses = Vec::new();
    let mut decoded = Vec::new();
    for (index, &(bytes, encoding)) in cases.iter().enumerate() {
        let mut reader = supplied(9511 + index as u64, bytes);
        let document = info(&mut reader);
        let witness = digest(&document).to_owned();
        assert_eq!(witness, Sha256::digest(bytes).to_hex());
        assert_eq!(document.get("encoding").text(), encoding);
        assert_eq!(reader.capture().bytes(), bytes);
        let window = json(reader.read_window(0, 64, || false).unwrap());
        assert!(window.get("has_replacements").flag());
        let text = window.get("text").text().to_owned();
        assert_ne!(witness, Sha256::digest(text.as_bytes()).to_hex(),
            "the witness must not hash the replacement-decoded view");
        let copy = json(reader.copy_range(0, bytes.len() as u64, || false).unwrap());
        assert_eq!(copy.get("original_hex").text(), hex(bytes));
        assert_eq!(digest(&info(&mut reader)), witness);
        witnesses.push(witness);
        decoded.push(text);
    }
    assert_eq!(decoded[0], decoded[1], "distinct malformed bytes display the same replacement text");
    assert_ne!(witnesses[0], witnesses[1]);
    assert_eq!(decoded[2], decoded[3], "the UTF-16 endiannesses display the same text");
    assert_ne!(witnesses[2], witnesses[3]);
}

#[test]
fn canonically_equivalent_unicode_stays_byte_distinct_and_owners_stay_independent() {
    let composed = "caf\u{e9}".as_bytes();
    let decomposed = "cafe\u{301}".as_bytes();
    let mut first = supplied(9521, composed);
    let mut second = supplied(9522, decomposed);
    let mut same_bytes = ReaderSession::from_bytes(owner(9523), Path::new("different-label.rs"),
        composed, || false).unwrap();
    let first_digest = digest(&info(&mut first)).to_owned();
    let second_digest = digest(&info(&mut second)).to_owned();
    assert_eq!(first_digest, Sha256::digest(composed).to_hex());
    assert_eq!(second_digest, Sha256::digest(decomposed).to_hex());
    assert_ne!(first_digest, second_digest, "no Unicode normalization may change byte identity");
    assert_eq!(digest(&info(&mut same_bytes)), first_digest,
        "a digest binds content, independently of the owner and path label");
    assert_ne!(first.capture().request(), same_bytes.capture().request());
    assert_ne!(first.capture().bytes().as_ptr(), same_bytes.capture().bytes().as_ptr(),
        "equal witnesses must not silently merge independently supplied sources");
}

#[test]
fn same_length_replacement_and_parent_close_cannot_change_a_pinned_witness() {
    let root = fixture();
    let path = root.join("source.rs");
    fs::write(&path, b"abc").unwrap();
    let mut original = ReaderSession::open(owner(9531), &path, 3, || false).unwrap();
    let first = info(&mut original);
    assert_eq!(digest(&first), ABC_SHA256);
    assert_eq!(first.get("capture_origin").text(), "regular-file-observation-not-atomic");
    assert_eq!(first.get("initial_source_bytes_read").number(), 3);
    assert!(first.get("initial_read_calls").number() > 0);

    fs::write(&path, b"abd").unwrap();
    let mut replacement = ReaderSession::open(owner(9532), &path, 3, || false).unwrap();
    assert_eq!(digest(&info(&mut replacement)), Sha256::digest(b"abd").to_hex());
    assert_ne!(digest(&info(&mut replacement)), ABC_SHA256);
    assert_eq!(digest(&info(&mut original)), ABC_SHA256);
    assert_eq!(replacement.capture().bytes().len(), original.capture().bytes().len());

    // The retained label now names a directory, so another regular-file read
    // cannot supply the following info, fork, search, window or copy results.
    fs::rename(&path, root.join("replacement.rs")).unwrap();
    fs::create_dir(&path).unwrap();
    let address = original.capture().bytes().as_ptr();
    let mut pin = original.fork(owner(9533), || false).unwrap();
    assert_eq!(pin.capture().bytes().as_ptr(), address);
    assert_ne!(pin.capture().request(), original.capture().request());
    drop(original);
    let document = info(&mut pin);
    assert_eq!(digest(&document), ABC_SHA256);
    assert_eq!(document.get("capture_origin").text(), "host-supplied");
    assert_eq!(document.get("initial_source_bytes_read").number(), 0);
    assert_eq!(document.get("initial_read_calls").number(), 0);
    assert_eq!(document.get("additional_source_bytes_read").number(), 0);
    pin.search(1, "bc", 4, 3, || false).unwrap();
    let copied = json(pin.copy_hit(1, 0, || false).unwrap());
    assert_eq!(copied.get("original_hex").text(), "6263");
    assert_eq!(copied.get("additional_source_bytes_read").number(), 0);
    let window = json(pin.read_window(0, 4, || false).unwrap());
    assert_eq!(window.get("text").text(), "abc");
    assert_eq!(window.get("additional_source_bytes_read").number(), 0);

    let mut next_pin = pin.fork(owner(9534), || false).unwrap();
    drop(pin);
    assert_eq!(next_pin.capture().bytes().as_ptr(), address);
    assert_eq!(digest(&info(&mut next_pin)), ABC_SHA256);
    assert_eq!(json(next_pin.copy_range(0, 3, || false).unwrap()).get("original_hex").text(), "616263");
}

#[test]
fn info_and_fork_do_not_reenter_a_source_sized_cancellable_scan() {
    let mut costs = Vec::new();
    for (index, length) in [0, 3, 3 * 64 * 1024 + 7].into_iter().enumerate() {
        let bytes = vec![b'a'; length];
        let mut reader = supplied(9541 + index as u64, &bytes);
        let expected = Sha256::digest(&bytes).to_hex();
        let mut info_polls = 0;
        let document = json(reader.info(|| { info_polls += 1; false }).unwrap());
        assert_eq!(digest(&document), expected);
        let mut fork_polls = 0;
        let mut pin = reader.fork(owner(9551 + index as u64), || { fork_polls += 1; false }).unwrap();
        assert_eq!(pin.capture().bytes().as_ptr(), reader.capture().bytes().as_ptr());
        assert_eq!(digest(&info(&mut pin)), expected);
        costs.push((info_polls, fork_polls));
    }
    assert!(costs[0].0 > 0 && costs[0].1 > 0, "metadata and sharing still honor cancellation");
    assert_eq!(costs[0], costs[1]);
    assert_eq!(costs[1], costs[2], "metadata and sharing must not rescan each source chunk");
}

#[test]
fn every_hash_cancellation_boundary_refuses_the_candidate_without_affecting_retained_bytes() {
    let mut empty_polls = 0;
    drop(ReaderSession::from_bytes(owner(9561), Path::new("empty"), b"",
        || { empty_polls += 1; false }).unwrap());
    let bytes = vec![b'a'; 3 * 64 * 1024 + 7];
    let mut full_polls = 0;
    let mut retained = ReaderSession::from_bytes(owner(9562), Path::new("retained"), &bytes,
        || { full_polls += 1; false }).unwrap();
    assert!(full_polls > empty_polls + 2, "a multi-chunk hash must offer cancellation inside its scan");
    let witness = digest(&info(&mut retained)).to_owned();
    let address = retained.capture().bytes().as_ptr();
    retained.search(7, "aaa", 1, bytes.len() as u64, || false).unwrap();
    for cancel_at in 1..=full_polls {
        let mut visited = 0;
        let candidate = ReaderSession::from_bytes(owner(9570 + cancel_at as u64), Path::new("candidate"),
            retained.capture().bytes(), || { visited += 1; visited == cancel_at });
        assert_eq!(candidate.err(), Some(ReaderSessionError::Canceled), "checkpoint {cancel_at}");
        assert_eq!(visited, cancel_at);
        assert_eq!(retained.capture().bytes().as_ptr(), address);
        assert_eq!(retained.accepted_generation(), Some(7));
        assert_eq!(digest(&info(&mut retained)), witness);
        assert_eq!(json(retained.copy_hit(7, 0, || false).unwrap()).get("original_hex").text(), "616161");
    }
    // The public API proves that no canceled candidate is published; its
    // per-session resource accounting is intentionally private to the reader.
    let mut accepted = supplied(9591, retained.capture().bytes());
    assert_eq!(digest(&info(&mut accepted)), witness);
    assert_ne!(accepted.capture().bytes().as_ptr(), address);
}

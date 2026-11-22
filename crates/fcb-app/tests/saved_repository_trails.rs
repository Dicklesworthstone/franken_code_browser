#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Explicit FCBT attachment, mixed-archive readiness and exact offline reader
//! installation through the production host. Code-first; execution belongs to
//! the independent strict-RCH batch verifier.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}, path::PathBuf};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotData, SnapshotEntry}, trail::{TrailBytes, TrailEntry, TrailError, TrailReadError, MAX_TRAIL_BYTES, MAX_TRAIL_ITEMS}};
use fcb::store::{Sha256, Sha256Digest};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession,
    trail::{SavedTrailError as Error, SavedTrailArchiveRequest, SavedTrailWriteParent, MAX_SAVED_TRAIL_PAGE}}};

fn range(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn visit<'a>(archive: Sha256Digest, path: &'a [u8], source: &[u8], selected: ByteRange,
    rationale: &'a str) -> TrailEntry<'a> {
    TrailEntry { archive, source: Sha256::digest(source), source_length: source.len() as u64,
        path, range: selected, rationale }
}
fn trail(fixture: &Fixture, name: &str, visits: &[TrailEntry<'_>]) -> PathBuf {
    let budget = ResourceBudget::new(owner(58000), ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = TrailBytes::encode(owner(58000), "Exact source tour", None, visits, &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = fixture.root.join(name); fs::write(&path, bytes.bytes()).unwrap(); path
}
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let receipt = json(&saved.info(|| false).unwrap());
    (receipt.get("member_bytes_read").number(), receipt.get("member_read_calls").number())
}

#[test]
fn mixed_archive_visits_keep_exact_order_rationale_and_metadata_only_readiness() {
    let source = b"fn retained() {}\n";
    let f = Fixture::new(&[entry(b"a.rs", source), SnapshotEntry { path: b"unavailable.rs",
        observed_bytes: 19, data: SnapshotData::Unavailable("SOURCE_NOT_CAPTURED") }], true);
    let mut saved = f.open(58001);
    let first = visit(saved.archive_digest(), b"a.rs", source, range(3, 11), "User rationale \"quoted\"\n<source>");
    let visits = [first,
        TrailEntry { archive: Sha256Digest::new([1; 32]), rationale: "Other comparison side", ..first },
        TrailEntry { path: b"missing.rs", rationale: "Missing exact member", ..first },
        TrailEntry { path: b"unavailable.rs", rationale: "Unavailable exact member", ..first },
        TrailEntry { source: Sha256Digest::new([2; 32]), rationale: "Never match by equal path", ..first },
        TrailEntry { rationale: "Intentional repeated visit", ..first }];
    let path = trail(&f, "mixed.fcbt", &visits);
    let before = reads(&mut saved);
    let page = json(&saved.attach_trail(1, &path, || false).unwrap());
    assert_eq!(reads(&mut saved), before);
    assert_eq!(page.get("total_items").number(), 6);
    assert_eq!(page.get("ready_items").number(), 2);
    assert_eq!(page.get("unavailable_items").number(), 4);
    assert_eq!(page.get("reference_bytes").number(), 48);
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
    assert!(!page.get("payload_verified").flag());
    assert!(!page.get("truncated").flag());
    for (item, (row, expected)) in page.get("items").array().iter().zip([
        "ready", "other-archive", "missing-member", "unavailable-member", "changed-source", "ready"]).enumerate() {
        assert_eq!(row.get("item").number(), item as u64);
        assert_eq!(row.get("readiness").text(), expected);
        assert_eq!(row.get("rationale").text(), visits[item].rationale);
        assert_eq!(row.get("evidence").text(), "explicit-user-selection");
        if expected != "ready" { assert_eq!(row.get("member"), &Json::Null); }
    }
    for item in 1..5 {
        assert!(saved.open_trail_reader(owner(58002), 1, item, || false).is_err());
    }
    assert_eq!(reads(&mut saved), before, "unready targets cannot trigger source I/O");
    let next = json(&saved.trail_page(1, 4, 2, || false).unwrap());
    assert_eq!(next.get("items").array(), &page.get("items").array()[4..]);
    assert_eq!(reads(&mut saved), before);
}

#[test]
fn raw_utf16_trail_opens_the_retained_capture_after_both_input_paths_move() {
    let raw = b"source-\xff.rs";
    let mut source = vec![0xff, 0xfe];
    for scalar in "head\r\nbanana\n".encode_utf16() { source.extend_from_slice(&scalar.to_le_bytes()); }
    let f = Fixture::new(&[entry(raw, &source)], true);
    let mut saved = f.open(58010);
    let item = visit(saved.archive_digest(), raw, &source, range(16, 22), "Preserve original UTF-16 bytes");
    let path = trail(&f, "unicode.fcbt", &[item]);
    saved.attach_trail(7, &path, || false).unwrap();
    fs::rename(&path, f.root.join("moved.fcbt")).unwrap();
    fs::write(&path, b"replacement is not a trail").unwrap();
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    fs::write(&f.path, b"replacement is not an archive").unwrap();
    let page = json(&saved.trail_page(7, 0, 1, || false).unwrap());
    assert_eq!(page.get("items").array()[0].get("path").get("hex").text(), hex(raw));
    let (mut reader, receipt) = saved.open_trail_reader(owner(58011), 7, 0, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(&source).to_hex());
    assert_eq!(receipt.get("selection").get("start").number(), 16);
    assert_eq!(receipt.get("selection").get("end").number(), 22);
    assert_eq!(receipt.get("path").get("hex").text(), hex(raw));
    assert_eq!(receipt.get("source_observation").text(), "verified-saved-member");
    assert!(!receipt.get("live_source_reopened").flag());
    saved.detach_trail(8, || false).unwrap();
    drop(saved);
    assert_eq!(reader.capture().bytes(), source);
    assert_eq!(json(&reader.copy_range(16, 22, || false).unwrap()).get("original_hex").text(), "61006e006100");
}

#[test]
fn large_ranges_and_empty_point_bookmarks_preserve_range_without_expanding_the_window() {
    let source = vec![b'a'; 192 * 1024];
    let f = Fixture::new(&[entry(b"large.rs", &source)], true);
    let mut saved = f.open(58020);
    let first = visit(saved.archive_digest(), b"large.rs", &source, range(100_000, source.len() as u64), "Full exact tail");
    let point = TrailEntry { range: range(source.len() as u64, source.len() as u64), rationale: "EOF bookmark", ..first };
    let path = trail(&f, "large.fcbt", &[first, point]);
    saved.attach_trail(u64::MAX, &path, || false).unwrap();
    for (item, expected) in [first, point].iter().enumerate() {
        let (reader, receipt) = saved.open_trail_reader(owner(58021 + item as u64), u64::MAX, item, || false).unwrap();
        let receipt = json(&receipt);
        assert_eq!(receipt.get("trail_generation").number(), u64::MAX);
        assert_eq!(receipt.get("selection").get("start").number(), expected.range.start().get());
        assert_eq!(receipt.get("selection").get("end").number(), expected.range.end().get());
        assert_eq!(reader.capture().bytes(), source);
        assert!(receipt.get("window").get("text").text().len() <= 16 * 1024);
    }
}

#[test]
fn failed_canceled_and_stale_replacements_preserve_the_previous_trail_and_other_queries() {
    let source = b"old captured source";
    let f = Fixture::new(&[entry(b"a.rs", source)], true);
    let mut saved = f.open(58030);
    let path = trail(&f, "first.fcbt", &[visit(saved.archive_digest(), b"a.rs", source, range(0, 3), "Keep me")]);
    saved.search(11, "captured", 2, || false).unwrap();
    let original = json(&saved.attach_trail(1, &path, || false).unwrap());
    assert!(saved.attach_trail(1, &path, || false).is_err());
    let bad = f.root.join("malformed.fcbt"); fs::write(&bad, b"FCBT-invalid").unwrap();
    assert!(saved.attach_trail(2, &bad, || false).is_err());
    let mut polls = 0;
    assert!(saved.attach_trail(3, &path, || { polls += 1; polls >= 4 }).err().unwrap().is_canceled());
    let recovered = json(&saved.trail_page(1, 0, 32, || false).unwrap());
    assert_eq!(recovered.get("trail_digest"), original.get("trail_digest"));
    assert_eq!(recovered.get("items"), original.get("items"));
    assert_eq!(recovered.get("last_trail_attempt").number(), 3);
    assert!(saved.results(11, 0, 2, || false).is_ok());
    assert!(saved.detach_trail(4, || true).err().unwrap().is_canceled());
    assert_eq!(saved.trail_generation(), Some(1));
    let detached = json(&saved.detach_trail(5, || false).unwrap());
    assert_eq!(detached.get("accepted_trail_generation"), &Json::Null);
    assert_eq!(detached.get("last_trail_attempt").number(), 5);
    assert_eq!(saved.trail_page(1, 0, 1, || false).err(), Some(Error::MissingTrail));
    assert!(saved.results(11, 0, 2, || false).is_ok());
}

#[test]
fn archived_payload_corruption_cannot_turn_metadata_readiness_into_a_verified_reader() {
    let source = b"Unique saved source payload before corruption";
    let f = Fixture::new(&[entry(b"a.rs", source)], true);
    fs::write(f.root.join("a.rs"), b"different live source is never a fallback").unwrap();
    let mut saved = f.open(58040);
    let path = trail(&f, "source.fcbt", &[visit(saved.archive_digest(), b"a.rs", source, range(1, 5), "Exact bytes only")]);
    saved.attach_trail(1, &path, || false).unwrap();
    let at = f.bytes.windows(source.len()).position(|window| window == source).unwrap() as u64;
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_trail_reader(owner(58041), 1, 0, || false).is_err());
    let page = json(&saved.trail_page(1, 0, 1, || false).unwrap());
    assert_eq!(page.get("items").array()[0].get("readiness").text(), "ready");
    assert!(!page.get("payload_verified").flag(), "metadata readiness is deliberately not payload availability");
}

#[test]
fn all_visits_paginate_without_loss_and_bounds_fail_without_changing_acceptance() {
    let f = Fixture::new(&[entry(b"a.rs", b"abc")], true);
    let mut saved = f.open(58050);
    let item = visit(saved.archive_digest(), b"a.rs", b"abc", range(1, 2), "Repeated visit");
    let path = trail(&f, "maximum.fcbt", &vec![item; MAX_TRAIL_ITEMS]);
    let first = json(&saved.attach_trail(1, &path, || false).unwrap());
    assert_eq!(first.get("items").array().len(), MAX_SAVED_TRAIL_PAGE);
    let mut start = 0;
    let mut seen = 0;
    loop {
        let page = json(&saved.trail_page(1, start, MAX_SAVED_TRAIL_PAGE, || false).unwrap());
        for row in page.get("items").array() {
            assert_eq!(row.get("item").number(), seen); seen += 1;
        }
        match page.get("next_offset") { Json::Null => break, value => start = value.number() as usize }
    }
    assert_eq!(seen, MAX_TRAIL_ITEMS as u64);
    for (start, limit) in [(0, 0), (0, MAX_SAVED_TRAIL_PAGE + 1), (MAX_TRAIL_ITEMS + 1, 1)] {
        assert_eq!(saved.trail_page(1, start, limit, || false).err(), Some(Error::InvalidLimits));
    }
    assert!(json(&saved.trail_page(1, MAX_TRAIL_ITEMS, 1, || false).unwrap()).get("items").array().is_empty());
    assert_eq!(saved.open_trail_reader(owner(58051), 1, MAX_TRAIL_ITEMS, || false).err(),
        Some(Error::Trail(TrailReadError::Format(TrailError::MissingItem))));
    let oversized = f.root.join("oversized.fcbt");
    fs::File::create(&oversized).unwrap().set_len(MAX_TRAIL_BYTES as u64 + 1).unwrap();
    assert_eq!(saved.attach_trail(2, &oversized, || false).err(), Some(Error::Trail(TrailReadError::Format(TrailError::Limit))));
    assert_eq!(saved.trail_generation(), Some(1));
}

#[test]
fn empty_trails_and_maximum_length_reference_pages_are_valid_explicit_states() {
    let f = Fixture::new(&[entry(b"a.rs", b"abc")], true);
    let mut saved = f.open(58060);
    let empty = trail(&f, "empty.fcbt", &[]);
    let empty = json(&saved.attach_trail(1, &empty, || false).unwrap());
    assert_eq!(empty.get("total_items").number(), 0);
    assert_eq!(empty.get("next_offset"), &Json::Null);
    assert!(empty.get("items").array().is_empty());
    let name = vec![b'x'; 16_384];
    let note = "\u{0001}".repeat(4096);
    let item = visit(Sha256Digest::new([9; 32]), &name, b"abc", range(0, 3), &note);
    let full = trail(&f, "full.fcbt", &vec![item; MAX_SAVED_TRAIL_PAGE]);
    let response = saved.attach_trail(2, &full, || false).unwrap();
    assert!(response.as_str().len() <= fcb_app::output::MAX_RESPONSE_BYTES);
    let page = json(&response);
    assert_eq!(page.get("items").array().len(), MAX_SAVED_TRAIL_PAGE);
    assert_eq!(page.get("items").array()[31].get("rationale").text(), note);
    assert_eq!(page.get("ready_items").number(), 0);
}


const SUPPLEMENTAL_SOURCE: &[u8] = b"\xff\xfea\0b\0\n\0";
fn supplemental_fixture(n: u64) -> (Fixture, Fixture, SavedRepositorySession, SavedTrailWriteParent, PathBuf) {
    let primary = Fixture::new(&[entry(b"src/raw-\xff.rs", b"primary source\n")], true);
    let other = Fixture::new(&[entry(b"a.rs", b"other first member"), entry(b"b.rs", b""),
        entry(b"src/raw-\xff.rs", SUPPLEMENTAL_SOURCE), SnapshotEntry {
            path: b"z.rs", observed_bytes: 8, data: SnapshotData::Unavailable("SOURCE_NOT_CAPTURED") }], false);
    let mut saved = primary.open(n);
    let other_digest = other.open(n + 1).archive_digest();
    let first = visit(saved.archive_digest(), b"src/raw-\xff.rs", b"primary source\n", range(0, 7), "Primary visit");
    let second = visit(other_digest, b"src/raw-\xff.rs", SUPPLEMENTAL_SOURCE, range(2, 6), "Other archive é\0λ");
    let path = trail(&primary, "supplemental-tour.fcbt", &[first, second,
        TrailEntry { range: range(3, 5), rationale: "Exact bytes inside UTF-16 units", ..second },
        TrailEntry { range: range(8, 8), rationale: "Point at end", ..second },
        TrailEntry { path: b"missing.rs", ..second },
        TrailEntry { source: Sha256Digest::new([8; 32]), ..second },
        TrailEntry { path: b"z.rs", ..second }]);
    let bytes = fs::read(&path).unwrap();
    let digest = fcb::search::trail::TrailView::open(&bytes, || false).unwrap().digest();
    saved.attach_trail(5, &path, || false).unwrap();
    (primary, other, saved, SavedTrailWriteParent { generation: 5, digest }, path)
}
fn supplemental_request<'a>(trail: SavedTrailWriteParent, item: usize, path: &'a std::path::Path)
    -> SavedTrailArchiveRequest<'a> {
    SavedTrailArchiveRequest { trail, item, archive: path, validation_bytes: 256 * 1024 * 1024 }
}

#[test]
fn explicit_supplemental_visits_preserve_primary_authority_and_independent_exact_readers() {
    let (primary, other, mut saved, trail, trail_path) = supplemental_fixture(58100);
    saved.search(12, "primary", 4, || false).unwrap();
    let page = json(&saved.trail_page(5, 0, 32, || false).unwrap());
    let primary_before = reads(&mut saved);
    fs::rename(&primary.path, primary.root.join("moved-primary.fcbs")).unwrap();
    fs::write(&primary.path, b"unrelated current pathname").unwrap();
    fs::rename(&trail_path, primary.root.join("moved-tour.fcbt")).unwrap();
    fs::write(&trail_path, b"unrelated current trail").unwrap();
    let mut retained = Vec::new(); let mut previous_file = 0;
    for (item, exact) in [(1, "61006200"), (2, "0062"), (3, "")] {
        let (mut reader, response) = saved.open_trail_reader_with_archive(owner(58110 + item as u64),
            supplemental_request(trail, item, &other.path), || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("command").text(), "trail-archive-open-reader");
        assert_eq!(receipt.get("archive_digest"), page.get("archive_digest"));
        assert_eq!(receipt.get("entry"), &page.get("items").array()[item]);
        assert_eq!(receipt.get("entry").get("readiness").text(), "other-archive");
        assert_eq!(receipt.get("entry").get("file_id"), &Json::Null);
        assert_eq!(receipt.get("source_archive").get("digest"), receipt.get("entry").get("archive_digest"));
        assert_eq!(receipt.get("source_archive").get("path").get("display").text(), other.path.to_str().unwrap());
        assert_eq!(receipt.get("source_archive").get("members_total").number(), 4);
        assert_eq!(receipt.get("source_archive").get("captured_members").number(), 3);
        assert!(!receipt.get("source_archive").get("discovery_complete").flag());
        let resolved = receipt.get("resolved_member");
        assert_eq!(resolved.get("member").number(), 2);
        assert_eq!(resolved.get("file_id").number(), 3);
        assert!(resolved.get("captured").flag());
        assert_eq!(resolved.get("source_digest"), receipt.get("source_digest"));
        assert_eq!(resolved.get("path"), receipt.get("entry").get("path"));
        assert_eq!(receipt.get("member"), resolved.get("member"));
        let file = receipt.get("file_id").number();
        assert!(file > receipt.get("members_total").number() && file > previous_file);
        assert_eq!(receipt.get("source_revision").number(), file); previous_file = file;
        assert_eq!(receipt.get("reader_owner").number(), 58110 + item as u64);
        assert_eq!(receipt.get("reader").get("owner").number(), 58110 + item as u64);
        assert_eq!(receipt.get("reader").get("file_id").number(), 1);
        assert_eq!(receipt.get("member_bytes_read").number(), SUPPLEMENTAL_SOURCE.len() as u64);
        assert_eq!(receipt.get("primary_member_bytes_read").number(), 0);
        assert_eq!(receipt.get("primary_member_read_calls").number(), 0);
        assert!(!receipt.get("archive_path_reopened").flag());
        assert!(!receipt.get("live_source_reopened").flag());
        assert!(receipt.get("accepted_trail_unchanged").flag());
        assert!(receipt.get("accepted_query_unchanged").flag());
        let validation = receipt.get("archive_validation");
        assert_eq!(validation.get("supplied_archives").number(), 2);
        assert_eq!(validation.get("validated_archives").number(), 2);
        assert_eq!(validation.get("distinct_archives").number(), 2);
        assert_eq!(validation.get("duplicate_archives").number(), 0);
        assert_eq!(validation.get("opened_archive_paths").number(), 1);
        assert_eq!(validation.get("validation_bytes_read").number(), other.bytes.len() as u64);
        assert_eq!(validation.get("validation_bytes_admitted").number(), other.bytes.len() as u64 + 1);
        assert!(validation.get("validation_complete").flag());
        assert_eq!(reader.capture().bytes(), SUPPLEMENTAL_SOURCE);
        let start = receipt.get("selection").get("start").number();
        let end = receipt.get("selection").get("end").number();
        assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(), exact);
        retained.push(reader);
    }
    assert_eq!(reads(&mut saved), primary_before);
    assert_eq!(json(&saved.trail_page(5, 0, 32, || false).unwrap()).get("items"), page.get("items"));
    assert!(saved.results(12, 0, 4, || false).is_ok());
    assert_eq!(saved.trail_generation(), Some(5));
    fs::rename(&other.path, other.root.join("moved-supplemental.fcbs")).unwrap();
    fs::write(&other.path, b"unrelated later bytes").unwrap();
    saved.detach_trail(6, || false).unwrap(); drop(saved);
    for reader in retained { assert_eq!(reader.capture().bytes(), SUPPLEMENTAL_SOURCE); }
}

#[test]
fn supplemental_reader_rejects_stale_wrong_or_unavailable_pins_without_primary_reads() {
    let (primary, other, mut saved, trail, _) = supplemental_fixture(58120);
    let before = reads(&mut saved);
    let missing = other.root.join("not-granted.fcbs");
    let invalid_parent = SavedTrailWriteParent { digest: Sha256Digest::new([7; 32]), ..trail };
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
        supplemental_request(SavedTrailWriteParent { generation: 6, ..trail }, 1, &missing), || false).err(),
        Some(Error::StaleGeneration));
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
        supplemental_request(invalid_parent, 1, &missing), || false).err(), Some(Error::ParentMismatch));
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
        supplemental_request(trail, 0, &missing), || false).err(), Some(Error::InvalidLimits));
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
        supplemental_request(trail, usize::MAX, &missing), || false).err(),
        Some(Error::Trail(TrailReadError::Format(TrailError::MissingItem))));
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
        supplemental_request(trail, 1, &primary.path), || false).err(),
        Some(Error::Trail(TrailReadError::OtherArchive)));
    for (item, error) in [(4, TrailReadError::MissingMember), (5, TrailReadError::ChangedSource),
        (6, TrailReadError::UnavailableMember)] {
        assert_eq!(saved.open_trail_reader_with_archive(owner(58122),
            supplemental_request(trail, item, &other.path), || false).err(), Some(Error::Trail(error)));
    }
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122), SavedTrailArchiveRequest {
        validation_bytes: other.bytes.len() as u64, ..supplemental_request(trail, 1, &other.path) }, || false).err(),
        Some(Error::ExportValidationByteLimit));
    assert_eq!(saved.open_trail_reader_with_archive(owner(58122), SavedTrailArchiveRequest {
        validation_bytes: 256 * 1024 * 1024 + 1, ..supplemental_request(trail, 1, &missing) }, || false).err(),
        Some(Error::InvalidLimits));
    assert_eq!(reads(&mut saved), before);
    assert_eq!(saved.trail_generation(), Some(5));
    let (reader, _) = saved.open_trail_reader_with_archive(owner(58123),
        supplemental_request(trail, 1, &other.path), || false).unwrap();
    assert_eq!(reader.capture().bytes(), SUPPLEMENTAL_SOURCE);
}

#[test]
fn supplemental_cancellation_and_corruption_never_publish_a_candidate_or_change_the_trail() {
    let (_, other, mut saved, trail, _) = supplemental_fixture(58130);
    let before = reads(&mut saved);
    let request = supplemental_request(trail, 1, &other.path);
    let mut complete_polls = 0;
    saved.open_trail_reader_with_archive(owner(58132), request, || { complete_polls += 1; false }).unwrap();
    for stop_at in [1, 2, complete_polls / 2, complete_polls - 1] {
        let mut polls = 0;
        let error = saved.open_trail_reader_with_archive(owner(58133), request, || {
            polls += 1; polls >= stop_at
        }).err().unwrap();
        assert!(error.is_canceled(), "stop_at={stop_at}, observed={polls}, error={error}");
        assert_eq!(saved.trail_generation(), Some(5));
    }
    let at = other.bytes.windows(SUPPLEMENTAL_SOURCE.len()).position(|part| part == SUPPLEMENTAL_SOURCE).unwrap();
    let mut corrupted = other.bytes.clone(); corrupted[at + 2] ^= 1;
    fs::write(&other.path, &corrupted).unwrap();
    assert!(saved.open_trail_reader_with_archive(owner(58133), request, || false).is_err());
    fs::write(&other.path, &other.bytes).unwrap();
    let (reader, _) = saved.open_trail_reader_with_archive(owner(58134), request, || false).unwrap();
    assert_eq!(reader.capture().bytes(), SUPPLEMENTAL_SOURCE);
    assert_eq!(reads(&mut saved), before);
}

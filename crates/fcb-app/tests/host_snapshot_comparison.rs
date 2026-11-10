#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real FCBS archives -> retained comparison -> exact reader regression source.
//! Code-first campaign: independent strict-RCH verification remains pending.
mod support;
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use fcb::search::paged_snapshot::{PagedSearchError, PagedSnapshotError};
use fcb::store::Sha256;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, reader::ReaderSession,
    snapshot_comparison::{SnapshotComparisonSession as Session, SnapshotComparisonError as Error,
        SnapshotComparisonSide as Side, SnapshotChangeFilter as Filter, SnapshotLimits,
        MAX_SNAPSHOT_COMPARISON_RESPONSE}};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn entry<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn unavailable<'a>(path: &'a [u8], observed_bytes: u64) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes, data: SnapshotData::Unavailable("READ_DENIED") }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

struct Fixture { root: PathBuf, before: PathBuf, after: PathBuf, old_bytes: Vec<u8>, limits: SnapshotLimits }
impl Fixture {
    fn new(before: &[SnapshotEntry<'_>], after: &[SnapshotEntry<'_>]) -> Self {
        Self::configured(before, after, [true, true], ["comparison-test-v1", "comparison-test-v1"], SnapshotLimits::default())
    }
    fn configured(before: &[SnapshotEntry<'_>], after: &[SnapshotEntry<'_>],
        complete: [bool; 2], policies: [&str; 2], limits: SnapshotLimits) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-snapshot-comparison-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let paths = [root.join("before.fcbs"), root.join("after.fcbs")];
        let budget = ResourceBudget::new(owner(58100), ByteLength::new(256 * 1024 * 1024)).unwrap();
        let mut old_bytes = Vec::new();
        for (index, entries) in [before, after].into_iter().enumerate() {
            let encoded = SnapshotBytes::encode(owner(58100), complete[index], policies[index], entries,
                limits, &budget, allocation(index as u64 + 1), || false).unwrap();
            fs::write(&paths[index], encoded.bytes()).unwrap();
            if index == 0 { old_bytes.extend_from_slice(encoded.bytes()); }
        }
        Self { root, before: paths[0].clone(), after: paths[1].clone(), old_bytes, limits }
    }
    fn open(&self, id: u64) -> Session {
        Session::open(owner(id), &self.before, &self.after, self.limits, || false).unwrap()
    }
}
fn finish(session: &mut Session, generation: u64, step: usize) -> Json {
    session.begin(generation, || false).unwrap();
    loop {
        let response = session.step(generation, step, || false).unwrap();
        let page = json(&response);
        assert!(page.get("last_step_entries").number() <= step as u64);
        if !page.get("comparison_in_progress").flag() { return page; }
    }
}
fn copy(reader: &mut ReaderSession) -> String {
    let bytes = reader.capture().bytes().len() as u64;
    json(&reader.copy_range(0, bytes, || false).unwrap()).get("original_hex").text().to_owned()
}
fn reads(session: &mut Session) -> [(u64, u64); 2] {
    let info = json(&session.info(|| false).unwrap());
    ["before_archive", "after_archive"].map(|side| {
        let side = info.get(side);
        (side.get("member_bytes_read").number(), side.get("member_read_calls").number())
    })
}

#[test]
fn real_archives_classify_all_paths_progressively_and_filters_preserve_global_ids() {
    let fixture = Fixture::new(
        &[entry(b"a.same", b"same"), entry(b"b.changed", b"before"), entry(b"d.removed", b"old"), unavailable(b"e.denied", 91)],
        &[entry(b"a.same", b"same"), entry(b"b.changed", b"after!"), entry(b"c.added", b"new"), entry(b"e.denied", b"now captured")]);
    let mut session = fixture.open((1 << 53) + 7);
    let info = json(&session.info(|| false).unwrap());
    assert_eq!(info.get("owner").number(), (1 << 53) + 7);
    assert_eq!(info.get("accepted_comparison_generation"), &Json::Null);
    for side in ["before_archive", "after_archive"] {
        let archive = info.get(side);
        assert_eq!(archive.get("members_total").number(), 4);
        assert_eq!(archive.get("archive_bytes"), archive.get("initial_archive_bytes_read"));
        assert!(archive.get("initial_archive_read_calls").number() > 0);
    }
    let generation = (1 << 53) + 9;
    let start = json(&session.begin(generation, || false).unwrap());
    assert_eq!(start.get("retained_entries").number(), 0);
    assert_eq!(start.get("pending_comparison_generation").number(), generation);
    assert_eq!(session.page(generation, Filter::All, 0, 8, || false).err(), Some(Error::MissingComparison));
    let first = json(&session.step(generation, 2, || false).unwrap());
    assert_eq!(first.get("compared_paths").number(), 2);
    assert!(first.get("comparison_in_progress").flag());
    session.step(generation, 2, || false).unwrap();
    let response = session.step(generation, 2, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let terminal = json(&response);
    assert!(terminal.get("scan_complete").flag());
    assert!(terminal.get("membership_comparable").flag());
    assert!(!terminal.get("comparison_complete").flag());
    assert_eq!(terminal.get("paths_upper_bound").number(), 8);
    assert_eq!(terminal.get("retained_entries").number(), 5);
    assert_eq!(terminal.get("known_differences").number(), 3);
    assert_eq!(terminal.get("uncertain_paths").number(), 1);
    assert_eq!(terminal.get("accepted_comparison_generation").number(), generation);
    assert_eq!(terminal.get("pending_comparison_generation"), &Json::Null);
    for kind in ["unchanged", "changed", "added", "removed", "unavailable"] { assert_eq!(terminal.get(kind).number(), 1); }
    let all = json(&session.page(generation, Filter::All, 0, 128, || false).unwrap());
    assert_eq!(all.get("entries").array().iter().map(|row| row.get("entry_id").number()).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
    let differences = json(&session.page(generation, Filter::Differences, 0, 2, || false).unwrap());
    assert_eq!(differences.get("matching_entries").number(), 4);
    assert_eq!(differences.get("next_offset").number(), 2);
    assert_eq!(differences.get("entries").array()[0].get("entry_id").number(), 2);
    let last = json(&session.page(generation, Filter::Differences, 2, 2, || false).unwrap());
    assert_eq!(last.get("entries").array()[0].get("entry_id").number(), 4);
    assert_eq!(last.get("next_offset"), &Json::Null);
    for (filter, id) in [(Filter::Unchanged, 1), (Filter::Changed, 2), (Filter::Added, 3), (Filter::Removed, 4), (Filter::Unavailable, 5)] {
        let page = json(&session.page(generation, filter, 0, 8, || false).unwrap());
        assert_eq!(page.get("entries").array()[0].get("entry_id").number(), id);
    }
    assert_eq!(reads(&mut session), [(0, 0), (0, 0)], "Classification and paging must not load source payloads");
}

#[test]
fn policy_and_opposite_membership_closure_control_added_removed_claims() {
    for (policies, complete, expected) in [
        (["same", "same"], [true, true], ["removed", "changed", "added"]),
        (["before-policy", "after-policy"], [true, true], ["only-before", "changed", "only-after"]),
        (["same", "same"], [false, true], ["removed", "changed", "only-after"]),
        (["same", "same"], [true, false], ["only-before", "changed", "added"]),
    ] {
        let fixture = Fixture::configured(&[entry(b"a", b"old"), entry(b"b", b"one")],
            &[entry(b"b", b"two"), entry(b"c", b"new")], complete, policies, SnapshotLimits::default());
        let mut session = fixture.open(58102);
        let result = finish(&mut session, 1, 1);
        let kinds: Vec<_> = result.get("entries").array().iter().map(|row| row.get("kind").text()).collect();
        assert_eq!(kinds, expected);
        assert_eq!(result.get("comparison_complete").flag(), policies[0] == policies[1] && complete == [true, true]);
        assert_eq!(reads(&mut session), [(0, 0), (0, 0)]);
    }
}

#[test]
fn empty_equal_and_unavailable_empty_are_distinct_terminal_observations() {
    let fixture = Fixture::new(&[], &[]);
    let mut session = fixture.open(58103);
    let terminal = finish(&mut session, 1, 1);
    assert!(terminal.get("comparison_complete").flag());
    assert_eq!(terminal.get("retained_entries").number(), 0);
    assert_eq!(terminal.get("steps").number(), 1);
    assert_eq!(session.step(1, 1, || false).unwrap().exit_code(), EXIT_OK);
    let equal = Fixture::new(&[entry(b"empty", b"")], &[entry(b"empty", b"")]);
    let mut session = equal.open(58104);
    let terminal = finish(&mut session, 1, 1);
    assert_eq!(terminal.get("unchanged").number(), 1);
    assert_eq!(terminal.get("entries").array().len(), 0);
    let unavailable = Fixture::new(&[unavailable(b"empty", 0)], &[entry(b"empty", b"")]);
    let mut session = unavailable.open(58105);
    let terminal = finish(&mut session, 1, 1);
    assert_eq!(terminal.get("unavailable").number(), 1);
    assert!(!terminal.get("comparison_complete").flag());
    assert_eq!(session.open_reader(owner(58106), 1, 1, Side::Before, || false).err(), Some(Error::UnavailableSide));
    let (mut reader, _) = session.open_reader(owner(58107), 1, 1, Side::After, || false).unwrap();
    assert_eq!(copy(&mut reader), "");
}

#[test]
fn raw_path_bytes_and_unicode_spellings_are_aligned_without_normalization() {
    let fixture = Fixture::new(
        &[entry(b"a\n\\name", b"same"), entry("e\u{301}.rs".as_bytes(), b"old"), entry(b"z\xff", b"old")],
        &[entry(b"a\n\\name", b"same"), entry(b"z\xfe", b"new"), entry("é.rs".as_bytes(), b"new")]);
    let mut session = fixture.open(58108);
    finish(&mut session, 1, 2);
    let page = json(&session.page(1, Filter::All, 0, 10, || false).unwrap());
    let paths: Vec<_> = page.get("entries").array().iter().map(|row| row.get("path").get("hex").text().to_owned()).collect();
    assert_eq!(paths, vec![hex(b"a\n\\name"), hex("e\u{301}.rs".as_bytes()), hex(b"z\xfe"), hex(b"z\xff"), hex("é.rs".as_bytes())]);
    assert_eq!(page.get("added").number(), 2);
    assert_eq!(page.get("removed").number(), 2);
    assert_ne!(paths[1], paths[4], "Canonical-equivalent display names are different archive members");
    let (mut reader, receipt) = session.open_reader(owner(58109), 1, 4, Side::Before, || false).unwrap();
    assert_eq!(json(&receipt).get("path").get("hex").text(), hex(b"z\xff"));
    assert_eq!(copy(&mut reader), hex(b"old"));
}

#[test]
fn exact_before_after_readers_survive_path_replacement_and_comparison_close() {
    let before = b"\xff\xfeA\0\r\0\n\0B\0";
    let after = b"new\0malformed\xff\r\n";
    let fixture = Fixture::new(&[entry(b"same.rs", before)], &[entry(b"same.rs", after)]);
    let mut session = fixture.open(58110);
    finish(&mut session, 1, 8);
    // Replace the pathname with a different valid archive. The service keeps
    // the original descriptor; it does not reopen either selected file.
    fs::rename(&fixture.before, fixture.root.join("retained-before.fcbs")).unwrap();
    fs::write(&fixture.before, fs::read(&fixture.after).unwrap()).unwrap();
    let (mut old, old_receipt) = session.open_reader(owner(58111), 1, 1, Side::Before, || false).unwrap();
    let (mut new, new_receipt) = session.open_reader(owner(58112), 1, 1, Side::After, || false).unwrap();
    let old_receipt = json(&old_receipt); let new_receipt = json(&new_receipt);
    assert_eq!(old_receipt.get("source_digest").text(), Sha256::digest(before).to_hex());
    assert_eq!(new_receipt.get("source_digest").text(), Sha256::digest(after).to_hex());
    assert_eq!(old_receipt.get("file_id").number(), 1);
    assert_eq!(new_receipt.get("file_id").number(), 65_537);
    assert_eq!(old_receipt.get("reader").get("encoding").text(), "utf16le");
    for receipt in [&old_receipt, &new_receipt] {
        assert_eq!(receipt.get("source_observation").text(), "verified-saved-member");
        assert!(!receipt.get("live_source_reopened").flag());
        assert_eq!(receipt.get("live_source_bytes_read").number(), 0);
        assert_eq!(receipt.get("reader").get("capture_origin").text(), "host-supplied");
        assert_eq!(receipt.get("reader").get("initial_source_bytes_read").number(), 0);
        assert_eq!(receipt.get("source_digest"), receipt.get("reader").get("capture_sha256"));
        assert_eq!(receipt.get("captured_bytes"), receipt.get("reader").get("captured_bytes"));
    }
    assert_eq!(reads(&mut session)[0].0, before.len() as u64);
    assert_eq!(reads(&mut session)[1].0, after.len() as u64);
    drop(session);
    assert_eq!(copy(&mut old), hex(before));
    assert_eq!(copy(&mut new), hex(after));
}

#[test]
fn rejected_ids_sides_pages_and_owner_reuse_do_not_load_members() {
    let fixture = Fixture::new(&[entry(b"before", b"old"), unavailable(b"denied", 32)],
        &[entry(b"after", b"new"), unavailable(b"denied", 32)]);
    let mut session = fixture.open(58113);
    finish(&mut session, 1, 8);
    assert_eq!(session.page(2, Filter::All, 0, 8, || false).err(), Some(Error::StaleGeneration));
    for (generation, id, side, expected) in [
        (2, 1, Side::After, Error::StaleGeneration),
        (1, 0, Side::After, Error::MissingEntry), (1, 4, Side::After, Error::MissingEntry),
        (1, 1, Side::Before, Error::UnavailableSide), (1, 2, Side::After, Error::UnavailableSide),
        (1, 3, Side::Before, Error::UnavailableSide), (1, 3, Side::After, Error::UnavailableSide),
    ] {
        assert_eq!(session.open_reader(owner(58114), generation, id, side, || false).err(), Some(expected));
    }
    assert_eq!(session.open_reader(owner(58113), 1, 1, Side::After, || false).err(), Some(Error::OwnerMismatch));
    for (start, limit) in [(0, 0), (0, 129), (4, 1)] {
        assert_eq!(session.page(1, Filter::All, start, limit, || false).err(), Some(Error::InvalidLimits));
    }
    for step in [0, 1025] { assert_eq!(session.step(1, step, || false).err(), Some(Error::InvalidLimits)); }
    assert_eq!(reads(&mut session), [(0, 0), (0, 0)]);
}

#[test]
fn replacement_generations_and_canceled_steps_preserve_prior_accepted_authority() {
    let fixture = Fixture::new(&[entry(b"a", b"old"), entry(b"b", b"same")],
        &[entry(b"a", b"new"), entry(b"b", b"same")]);
    let mut session = fixture.open(58115);
    finish(&mut session, 1, 8);
    session.begin(2, || false).unwrap();
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(session.pending_generation(), Some(2));
    assert_eq!(json(&session.page(1, Filter::Changed, 0, 8, || false).unwrap()).get("matching_entries").number(), 1);
    assert!(session.cancel_pending());
    assert!(!session.cancel_pending());
    assert_eq!(session.begin(2, || false).err(), Some(Error::StaleGeneration));
    session.begin(3, || false).unwrap();
    assert!(session.step(3, 8, || true).err().unwrap().is_canceled());
    assert_eq!(session.pending_generation(), None);
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(session.last_attempt(), 3);
    let terminal = finish(&mut session, 4, 1);
    assert_eq!(terminal.get("accepted_comparison_generation").number(), 4);
    assert_eq!(session.page(1, Filter::All, 0, 8, || false).err(), Some(Error::StaleGeneration));
}

#[test]
fn final_publication_cancellation_burns_attempts_without_accepting_candidate_state() {
    let fixture = Fixture::new(&[entry(b"a", b"old"), entry(b"b", b"same")],
        &[entry(b"a", b"new"), entry(b"b", b"same")]);
    let mut probe = fixture.open(58116);
    finish(&mut probe, 1, 8);
    let mut begin_polls = 0;
    probe.begin(2, || { begin_polls += 1; false }).unwrap();
    let mut step_polls = 0;
    probe.step(2, 8, || { step_polls += 1; false }).unwrap();
    assert!(begin_polls >= 3 && step_polls > begin_polls);

    let mut session = fixture.open(58117);
    finish(&mut session, 1, 8);
    let mut calls = 0;
    let error = session.begin(2, || { calls += 1; calls == begin_polls }).err().unwrap();
    assert!(error.is_canceled());
    assert_eq!(session.last_attempt(), 2);
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(session.pending_generation(), None);
    session.begin(3, || false).unwrap();
    calls = 0;
    let error = session.step(3, 8, || { calls += 1; calls == step_polls }).err().unwrap();
    assert!(error.is_canceled());
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(session.pending_generation(), None);
    assert_eq!(json(&session.page(1, Filter::Changed, 0, 8, || false).unwrap()).get("entries").array().len(), 1);
    assert_eq!(reads(&mut session), [(0, 0), (0, 0)]);
    assert_eq!(session.begin(3, || false).err(), Some(Error::StaleGeneration));
}

#[test]
fn in_place_archive_mutation_is_detected_before_reader_authority_is_published() {
    let original = b"BEFORE_UNIQUE_MUTATION_TOKEN";
    let fixture = Fixture::new(&[entry(b"a", original)], &[entry(b"a", b"after")]);
    let mut session = fixture.open(58118);
    finish(&mut session, 1, 8);
    let (mut retained, _) = session.open_reader(owner(58119), 1, 1, Side::Before, || false).unwrap();
    let positions: Vec<_> = fixture.old_bytes.windows(original.len()).enumerate()
        .filter_map(|(offset, bytes)| (bytes == original).then_some(offset)).collect();
    assert_eq!(positions.len(), 1);
    let mut writer = OpenOptions::new().write(true).open(&fixture.before).unwrap();
    writer.seek(SeekFrom::Start(positions[0] as u64)).unwrap();
    writer.write_all(b"X").unwrap(); writer.sync_all().unwrap();
    assert_eq!(session.open_reader(owner(58120), 1, 1, Side::Before, || false).err(),
        Some(Error::Query(PagedSearchError::Archive(PagedSnapshotError::Changed))));
    // The accepted comparison describes the originally verified observations;
    // a changed descriptor cannot replace a readable already-transferred side.
    assert_eq!(copy(&mut retained), hex(original));
    assert_eq!(session.accepted_generation(), Some(1));
    let after = session.open_reader(owner(58121), 1, 1, Side::After, || false).unwrap().0;
    assert_eq!(after.capture().bytes(), b"after");
}

#[test]
fn invalid_archive_limits_and_cancellation_do_not_produce_a_half_open_pair() {
    let fixture = Fixture::new(&[entry(b"a", b"old")], &[entry(b"a", b"new")]);
    for limits in [
        SnapshotLimits { max_files: 65_537, ..fixture.limits },
        SnapshotLimits { max_file_bytes: 4 * 1024 * 1024 + 1, ..fixture.limits },
        SnapshotLimits { max_path_bytes: 16_385, ..fixture.limits },
    ] {
        assert_eq!(Session::open(owner(58122), &fixture.before, &fixture.after, limits, || false).err(), Some(Error::InvalidLimits));
    }
    assert!(Session::open(owner(58122), &fixture.before, &fixture.after, fixture.limits, || true).err().unwrap().is_canceled());
    fs::write(&fixture.after, b"not an FCBS archive").unwrap();
    assert!(Session::open(owner(58122), &fixture.before, &fixture.after, fixture.limits, || false).is_err());
    fs::write(&fixture.after, &fixture.old_bytes).unwrap();
    let mut repaired = fixture.open(58122);
    assert!(finish(&mut repaired, 1, 8).get("comparison_complete").flag());
}

#[test]
fn long_raw_paths_shorten_pages_without_losing_rows_or_next_page_authority() {
    let mut paths = Vec::new();
    for index in 0..128 {
        let mut path = format!("{index:04}-").into_bytes();
        path.resize(16_384, 0xff);
        paths.push(path);
    }
    let entries: Vec<_> = paths.iter().map(|path| entry(path, b"")).collect();
    let limits = SnapshotLimits { max_path_bytes: 16_384, ..SnapshotLimits::default() };
    let fixture = Fixture::configured(&entries, &entries, [true, true], ["same", "same"], limits);
    let mut session = fixture.open(58123);
    finish(&mut session, 1, 1024);
    let mut offset = 0u64;
    let mut ids = Vec::new();
    let mut shortened = false;
    loop {
        let response = session.page(1, Filter::All, offset as usize, 128, || false).unwrap();
        assert!(response.as_str().len() <= MAX_SNAPSHOT_COMPARISON_RESPONSE);
        let page = json(&response);
        let rows = page.get("entries").array();
        assert!(!rows.is_empty());
        shortened |= page.get("page_limited").flag();
        ids.extend(rows.iter().map(|row| row.get("entry_id").number()));
        let end = offset + rows.len() as u64;
        match page.get("next_offset") {
            Json::Null => { assert_eq!(end, 128); break; }
            next => { assert_eq!(next.number(), end); offset = end; }
        }
    }
    assert!(shortened);
    assert_eq!(ids, (1..=128).collect::<Vec<_>>());
    assert_eq!(reads(&mut session), [(0, 0), (0, 0)]);
}

#[test]
fn generation_exhaustion_and_empty_terminal_pages_never_reuse_old_authority() {
    let fixture = Fixture::new(&[entry(b"a", b"same")], &[entry(b"a", b"same")]);
    let mut session = fixture.open(58124);
    finish(&mut session, u64::MAX, 1);
    assert_eq!(session.begin(u64::MAX, || false).err(), Some(Error::StaleGeneration));
    assert_eq!(session.begin(1, || false).err(), Some(Error::StaleGeneration));
    assert_eq!(session.begin(0, || false).err(), Some(Error::StaleGeneration));
    let page = json(&session.page(u64::MAX, Filter::All, 1, 1, || false).unwrap());
    assert_eq!(page.get("entries").array().len(), 0);
    assert_eq!(page.get("next_offset"), &Json::Null);
    assert!(!page.get("page_limited").flag());
    assert_eq!(session.last_attempt(), u64::MAX);
}

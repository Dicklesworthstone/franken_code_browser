#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production exact-selection/immutable-append/edit/create-only/recovery contracts.
//! Code-first: execution belongs to independent strict-RCH batch verification.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry, owner};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}, os::unix::fs::PermissionsExt, path::Path};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{ResourceAllocationId, ResourceBudget,
    snapshot::{SnapshotData, SnapshotEntry}, trail::{TrailBytes, TrailEdit, TrailEntry, TrailView, MAX_TRAIL_ITEMS}};
use fcb::store::{Sha256, Sha256Digest};
use fcb_app::{EXIT_OK, EXIT_ERROR, EXIT_CANCELED};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession,
    trail::{SavedTrailError as Error, SavedTrailEditRequest, SavedTrailWriteParent, SavedTrailWriteRequest}}};

fn range(start: u64, end: u64) -> ByteRange { ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn request(destination: &Path, operation: u64) -> SavedTrailWriteRequest<'_> {
    SavedTrailWriteRequest { operation, parent: None, member: 0, selection: range(1, 4),
        title: "Exact source tour", rationale: "Why this source matters", destination }
}
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let value = json(&saved.info(|| false).unwrap());
    (value.get("member_bytes_read").number(), value.get("member_read_calls").number())
}
fn previous(f: &Fixture, title: &str, entries: &[TrailEntry<'_>]) -> (std::path::PathBuf, Sha256Digest) {
    let budget = ResourceBudget::new(owner(63500), ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = TrailBytes::encode(owner(63500), title, None, entries, &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = f.root.join("parent.fcbt"); fs::write(&path, bytes.bytes()).unwrap(); (path, bytes.digest())
}

#[test]
fn new_trail_pins_exact_original_bytes_with_raw_path_point_and_nul_text() {
    let source = [0xff, 0xfe, b'a', 0, b'b', 0, b'c', 0, b'\r', 0, b'\n', 0];
    let f = Fixture::new(&[entry(b"src/raw-\xff.rs", &source)], true);
    let mut saved = f.open(63501);
    let destination = f.root.join("new.fcbt");
    let req = SavedTrailWriteRequest { title: "Exact\0UTF-16 tour", rationale: "A\0note\n<user> \"λ\"",
        selection: range(4, 4), ..request(&destination, 1) };
    let response = saved.write_trail(req, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let receipt = json(&response);
    assert_eq!(receipt.get("command").text(), "trail-write");
    assert_eq!(receipt.get("mode").text(), "new");
    assert_eq!(receipt.get("operation").number(), 1);
    assert_eq!(receipt.get("parent_digest"), &Json::Null);
    assert_eq!(receipt.get("parent_generation"), &Json::Null);
    assert_eq!(receipt.get("parent_items").number(), 0);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(&source).to_hex());
    assert_eq!(receipt.get("source_bytes").number(), source.len() as u64);
    assert_eq!(receipt.get("member_bytes_read").number(), source.len() as u64);
    assert_eq!(receipt.get("reference_bytes").number(), 0);
    assert_eq!(receipt.get("title").text(), req.title);
    assert_eq!(receipt.get("rationale").text(), req.rationale);
    assert_eq!(receipt.get("path").get("hex").text(), "7372632f7261772dff2e7273");
    assert!(receipt.get("source_verified").flag());
    assert!(receipt.get("complete_artifact").flag());
    assert!(receipt.get("attach_eligible").flag());
    for key in ["source_payload_embedded", "archive_path_reopened", "live_source_reopened",
        "trail_attached", "overwritten", "power_loss_qualified", "truncated"] { assert!(!receipt.get(key).flag()); }
    assert_eq!(receipt.get("bytes_written"), receipt.get("trail_bytes"));
    let bytes = fs::read(&destination).unwrap();
    let trail = TrailView::open(&bytes, || false).unwrap();
    assert_eq!(trail.title(), req.title); assert_eq!(trail.len(), 1);
    assert_eq!(trail.digest().to_hex(), receipt.get("trail_digest").text());
    let visit = trail.entry(0).unwrap();
    assert_eq!(visit.archive, saved.archive_digest()); assert_eq!(visit.path, b"src/raw-\xff.rs");
    assert_eq!(visit.source, Sha256::digest(&source)); assert_eq!(visit.source_length, source.len() as u64);
    assert_eq!(visit.range, range(4, 4)); assert_eq!(visit.rationale, req.rationale);
    assert_eq!(saved.trail_generation(), None);
    assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o077, 0);
    let before = reads(&mut saved);
    assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), response.as_str());
    assert_eq!(reads(&mut saved), before);
    saved.attach_trail(1, &destination, || false).unwrap();
    let (reader, opened) = saved.open_trail_reader(owner(63502), 1, 0, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    assert_eq!(json(&opened).get("selection").get("start").number(), 4);
    assert_eq!(json(&opened).get("selection").get("end").number(), 4);
}

#[test]
fn append_preserves_every_mixed_archive_visit_and_original_notes_without_reopening_parent() {
    let source = b"first exact member\n";
    let f = Fixture::new(&[entry(b"a.rs", source)], true); let mut saved = f.open(63510);
    let selected = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(source),
        source_length: source.len() as u64, path: b"a.rs", range: range(1, 4), rationale: "Repeated user's note" };
    let other = TrailEntry { archive: Sha256Digest::new([7; 32]), path: b"other.rs", rationale: "Keep other archive", ..selected };
    let (parent_path, digest) = previous(&f, "Unchanged parent\0title", &[selected, other, selected]);
    let original = fs::read(&parent_path).unwrap();
    saved.attach_trail(9, &parent_path, || false).unwrap();
    saved.search(5, "exact", 8, || false).unwrap();
    fs::rename(&parent_path, f.root.join("original-parent.fcbt")).unwrap();
    fs::write(&parent_path, b"unrelated file now at parent pathname").unwrap();
    let destination = f.root.join("appended.fcbt");
    let req = SavedTrailWriteRequest { parent: Some(SavedTrailWriteParent { generation: 9, digest }),
        title: "Unchanged parent\0title", selection: range(0, source.len() as u64),
        rationale: "Entire saved member, not a new capture", ..request(&destination, 41) };
    let response = saved.write_trail(req, || false).unwrap(); let receipt = json(&response);
    assert_eq!(receipt.get("mode").text(), "append"); assert_eq!(receipt.get("parent_items").number(), 3);
    assert_eq!(receipt.get("parent_generation").number(), 9);
    assert_eq!(receipt.get("parent_digest").text(), digest.to_hex());
    assert_eq!(receipt.get("reference_bytes").number(), 9 + source.len() as u64);
    let bytes = fs::read(&destination).unwrap(); let trail = TrailView::open(&bytes, || false).unwrap();
    assert_eq!(trail.title(), req.title); assert_eq!(trail.parent_digest(), Some(digest)); assert_eq!(trail.len(), 4);
    for (item, visit) in [selected, other, selected].iter().enumerate() { assert_eq!(&trail.entry(item).unwrap(), visit); }
    assert_eq!(trail.entry(3).unwrap().range, req.selection);
    assert_eq!(trail.entry(3).unwrap().rationale, req.rationale);
    assert_eq!(fs::read(f.root.join("original-parent.fcbt")).unwrap(), original);
    assert_eq!(saved.trail_generation(), Some(9)); assert_eq!(saved.accepted_generation(), Some(5));
    assert_eq!(json(&saved.trail_page(9, 0, 32, || false).unwrap()).get("total_items").number(), 3);
    let refused = saved.write_trail(SavedTrailWriteRequest { operation: 42, destination: &parent_path, ..req }, || false).unwrap();
    assert_eq!(refused.exit_code(), EXIT_ERROR);
    assert_eq!(json(&refused).get("error").get("code").text(), "SAVED_TRAIL_DESTINATION_EXISTS");
    assert_eq!(json(&refused).get("effect").text(), "none");
    assert!(!json(&refused).get("complete_artifact").flag());
    assert_eq!(fs::read(&parent_path).unwrap(), b"unrelated file now at parent pathname");
    assert_eq!(saved.trail_generation(), Some(9));
}

#[test]
fn source_is_verified_through_the_retained_archive_descriptor_after_path_replacement() {
    let f = Fixture::new(&[entry(b"a.rs", b"original source")], true);
    let other = Fixture::new(&[entry(b"a.rs", b"changed  source")], true);
    let mut saved = f.open(63520); let original = saved.archive_digest();
    fs::rename(&f.path, f.root.join("retained.fcbs")).unwrap(); fs::write(&f.path, &other.bytes).unwrap();
    let destination = f.root.join("retained.fcbt");
    let response = saved.write_trail(request(&destination, 1), || false).unwrap();
    assert_eq!(json(&response).get("archive_digest").text(), original.to_hex());
    assert_eq!(json(&response).get("source_digest").text(), Sha256::digest(b"original source").to_hex());
    saved.attach_trail(1, &destination, || false).unwrap();
    assert_eq!(saved.open_trail_reader(owner(63521), 1, 0, || false).unwrap().0.capture().bytes(), b"original source");
    let mut replaced = f.open(63522);
    let page = replaced.attach_trail(1, &destination, || false).unwrap();
    assert!(page.as_str().contains("other-archive"));
}

#[test]
fn invalid_requests_preserve_previous_receipt_and_never_load_source_or_write() {
    let f = Fixture::new(&[entry(b"a.rs", b"member source")], true); let mut saved = f.open(63530);
    assert_eq!(saved.trail_write_receipt(1, || false).err(), Some(Error::MissingWrite));
    let first_path = f.root.join("first.fcbt");
    let first = saved.write_trail(request(&first_path, 1), || false).unwrap();
    saved.attach_trail(3, &first_path, || false).unwrap();
    let digest = Sha256Digest::from_hex(json(&first).get("trail_digest").text()).unwrap();
    let before = reads(&mut saved); let destination = f.root.join("must-not-exist.fcbt");
    let title = "x".repeat(257); let note = "x".repeat(4097);
    for req in [
        SavedTrailWriteRequest { title: &title, ..request(&destination, 2) },
        SavedTrailWriteRequest { rationale: &note, ..request(&destination, 3) },
        SavedTrailWriteRequest { selection: range(0, 99), ..request(&destination, 4) },
        SavedTrailWriteRequest { member: 99, ..request(&destination, 5) },
        SavedTrailWriteRequest { parent: Some(SavedTrailWriteParent { generation: 2, digest }), ..request(&destination, 6) },
        SavedTrailWriteRequest { parent: Some(SavedTrailWriteParent { generation: 3, digest: Sha256Digest::new([8; 32]) }), ..request(&destination, 7) },
        SavedTrailWriteRequest { parent: Some(SavedTrailWriteParent { generation: 3, digest }), title: "Changed title", ..request(&destination, 8) },
    ] {
        assert!(saved.write_trail(req, || false).is_err()); assert!(!destination.exists());
        assert_eq!(reads(&mut saved), before);
        assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), first.as_str());
    }
    assert_eq!(saved.last_trail_write_attempt(), 8); assert_eq!(saved.last_trail_write_operation(), Some(1));
    assert_eq!(saved.write_trail(request(&destination, 8), || false).err(), Some(Error::StaleWrite));
    assert_eq!(saved.trail_generation(), Some(3));
    let info = json(&saved.info(|| false).unwrap());
    assert_eq!(info.get("last_trail_write_attempt").number(), 8);
    assert_eq!(info.get("last_trail_write_operation").number(), 1);
}

#[test]
fn unavailable_members_and_full_parent_refuse_instead_of_fabricating_or_evicting_visits() {
    let f = Fixture::new(&[entry(b"a.rs", b"retained"), SnapshotEntry {
        path: b"b.rs", observed_bytes: 32, data: SnapshotData::Unavailable("DENIED") }], true);
    let mut saved = f.open(63540); let destination = f.root.join("refused.fcbt");
    assert!(saved.write_trail(SavedTrailWriteRequest { member: 1, ..request(&destination, 1) }, || false).is_err());
    let visit = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(b"retained"),
        source_length: 8, path: b"a.rs", range: range(0, 1), rationale: "Keep all notes" };
    let (path, digest) = previous(&f, "Full trail", &vec![visit; MAX_TRAIL_ITEMS]);
    saved.attach_trail(1, &path, || false).unwrap(); let before = reads(&mut saved);
    assert!(saved.write_trail(SavedTrailWriteRequest { title: "Full trail",
        parent: Some(SavedTrailWriteParent { generation: 1, digest }), ..request(&destination, 2) }, || false).is_err());
    assert!(!destination.exists()); assert_eq!(reads(&mut saved), before);
    assert_eq!(json(&saved.trail_page(1, 0, 32, || false).unwrap()).get("total_items").number(), MAX_TRAIL_ITEMS as u64);
}

#[test]
fn canceled_before_admission_preserves_old_receipt_while_engine_cancellation_is_retained() {
    let f = Fixture::new(&[entry(b"a.rs", b"retained source")], true); let mut saved = f.open(63550);
    let first_path = f.root.join("first.fcbt");
    let first = saved.write_trail(request(&first_path, 1), || false).unwrap();
    let destination = f.root.join("canceled.fcbt");
    assert!(saved.write_trail(request(&destination, 2), || true).err().unwrap().is_canceled());
    assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), first.as_str());
    assert_eq!(saved.last_trail_write_attempt(), 2); assert!(!destination.exists());
    let mut polls = 0;
    let failed = saved.write_trail(request(&destination, 3), || { polls += 1; polls == 2 }).unwrap();
    assert_eq!(failed.exit_code(), EXIT_CANCELED); let receipt = json(&failed);
    assert_eq!(receipt.get("effect").text(), "none"); assert_eq!(receipt.get("trail_digest"), &Json::Null);
    assert_eq!(receipt.get("total_items"), &Json::Null); assert_eq!(receipt.get("trail_bytes").number(), 0);
    assert!(!receipt.get("source_verified").flag()); assert!(!receipt.get("complete_artifact").flag());
    assert_eq!(saved.trail_write_receipt(3, || false).unwrap().as_str(), failed.as_str());
    assert!(!destination.exists());
}

#[test]
fn cancellation_after_create_or_complete_write_retains_exact_effect_without_replay() {
    for (complete, session) in [(false, 63560), (true, 63561)] {
        let f = Fixture::new(&[entry(b"a.rs", b"retained source")], true); let mut saved = f.open(session);
        let destination = f.root.join("canceled.fcbt");
        let response = saved.write_trail(request(&destination, 1), || {
            fs::metadata(&destination).is_ok_and(|m| !complete || m.len() > 0)
        }).unwrap();
        assert_eq!(response.exit_code(), EXIT_CANCELED); let receipt = json(&response);
        assert_eq!(receipt.get("effect").text(), if complete { "complete-file-sync-unconfirmed" } else { "destination-created-incomplete" });
        assert_eq!(receipt.get("attach_eligible").flag(), complete);
        assert_eq!(receipt.get("complete_artifact").flag(), complete);
        assert_eq!(receipt.get("bytes_written").number(), if complete { receipt.get("trail_bytes").number() } else { 0 });
        assert!(receipt.get("source_verified").flag());
        let before = reads(&mut saved);
        fs::rename(&destination, f.root.join("preserved-effect.fcbt")).unwrap();
        fs::write(&destination, b"an unrelated replacement").unwrap();
        drop(response);
        assert_eq!(json(&saved.trail_write_receipt(1, || false).unwrap()), receipt);
        assert_eq!(reads(&mut saved), before); assert_eq!(fs::read(&destination).unwrap(), b"an unrelated replacement");
        assert_eq!(saved.write_trail(request(&destination, 1), || false).err(), Some(Error::StaleWrite));
    }
}

#[test]
fn partial_multichunk_trail_write_is_not_reported_as_complete() {
    let source = b"retained"; let f = Fixture::new(&[entry(b"a.rs", source)], true); let mut saved = f.open(63570);
    let note = "r".repeat(4096);
    let visit = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(source), source_length: source.len() as u64,
        path: b"a.rs", range: range(0, 1), rationale: &note };
    let (path, digest) = previous(&f, "Large note trail", &vec![visit; 20]);
    saved.attach_trail(1, &path, || false).unwrap();
    let destination = f.root.join("partial.fcbt");
    let response = saved.write_trail(SavedTrailWriteRequest { title: "Large note trail",
        parent: Some(SavedTrailWriteParent { generation: 1, digest }), ..request(&destination, 1) },
        || fs::metadata(&destination).is_ok_and(|m| m.len() > 0)).unwrap();
    let receipt = json(&response); assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(receipt.get("bytes_written").number(), 64 * 1024);
    assert!(receipt.get("trail_bytes").number() > 64 * 1024);
    assert!(!receipt.get("complete_artifact").flag()); assert!(!receipt.get("attach_eligible").flag());
    assert_eq!(saved.trail_generation(), Some(1));
    assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn in_place_payload_damage_retains_failure_and_never_publishes_unverified_selection() {
    let source = b"unique retained saved source"; let f = Fixture::new(&[entry(b"a.rs", source)], true);
    let mut saved = f.open(63580); saved.search(1, "retained", 4, || false).unwrap();
    let (reader, _) = saved.open_member_reader(owner(63581), 0, || false).unwrap();
    let at = f.bytes.windows(source.len()).position(|window| window == source).unwrap();
    let mut file = OpenOptions::new().write(true).open(&f.path).unwrap();
    file.seek(SeekFrom::Start(at as u64)).unwrap(); file.write_all(b"X").unwrap(); file.flush().unwrap();
    let destination = f.root.join("must-not-publish.fcbt");
    let response = saved.write_trail(request(&destination, 1), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_ERROR); let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "none"); assert_eq!(receipt.get("trail_digest"), &Json::Null);
    assert!(!receipt.get("source_verified").flag()); assert!(!destination.exists());
    assert_eq!(reader.capture().bytes(), source); assert_eq!(saved.accepted_generation(), Some(1));
    assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), response.as_str());
}


#[test]
fn edits_preserve_mixed_archive_visits_and_only_change_the_reviewed_metadata_or_order() {
    let source = b"original retained source\n";
    let f = Fixture::new(&[entry(b"a.rs", source), SnapshotEntry {
        path: b"b.rs", observed_bytes: 32, data: SnapshotData::Unavailable("DENIED") }], true);
    let mut saved = f.open(63600);
    let selected = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(source),
        source_length: source.len() as u64, path: b"a.rs", range: range(1, 4), rationale: "First note" };
    let foreign = TrailEntry { archive: Sha256Digest::new([7; 32]), path: b"other-\xff.rs",
        range: range(5, 10), rationale: "Keep unavailable archive", ..selected };
    let unavailable = TrailEntry { path: b"b.rs", source_length: 32, range: range(20, 20),
        rationale: "Preserve point bookmark", ..selected };
    let visits = [selected, foreign, selected, unavailable];
    let (parent_path, digest) = previous(&f, "Original\0title", &visits);
    let original = fs::read(&parent_path).unwrap();
    saved.attach_trail(9, &parent_path, || false).unwrap();
    saved.search(7, "retained", 8, || false).unwrap();
    let before = reads(&mut saved);
    fs::rename(&parent_path, f.root.join("original-parent.fcbt")).unwrap();
    fs::write(&parent_path, b"unrelated replacement at the old trail path").unwrap();
    // Corrupt the retained archive payload as an independent oracle: a metadata
    // edit must not load it, even when its referenced source would now fail.
    let at = f.bytes.windows(source.len()).position(|window| window == source).unwrap();
    let mut file = OpenOptions::new().write(true).open(&f.path).unwrap();
    file.seek(SeekFrom::Start(at as u64)).unwrap(); file.write_all(b"X").unwrap(); file.flush().unwrap();

    let edits = [
        TrailEdit::Rename { title: "Revised\0title λ" },
        TrailEdit::SetRationale { ordinal: 1, rationale: "Corrected\0foreign note\nλ" },
        TrailEdit::Move { ordinal: 3, destination: 1 },
        TrailEdit::Remove { ordinal: 0 },
    ];
    for (index, edit) in edits.into_iter().enumerate() {
        let operation = index as u64 + 1;
        let destination = f.root.join(format!("edited-{operation}.fcbt"));
        let response = saved.edit_trail(SavedTrailEditRequest { operation,
            parent: SavedTrailWriteParent { generation: 9, digest }, edit, destination: &destination }, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let receipt = json(&response);
        assert_eq!(receipt.get("command").text(), "trail-edit"); assert_eq!(receipt.get("mode").text(), "edit");
        assert_eq!(receipt.get("operation").number(), operation);
        assert_eq!(receipt.get("parent_generation").number(), 9); assert_eq!(receipt.get("parent_items").number(), 4);
        assert_eq!(receipt.get("parent_digest").text(), digest.to_hex());
        assert!(receipt.get("source_anchors_unchanged").flag()); assert!(!receipt.get("source_verified").flag());
        assert_eq!(receipt.get("member_bytes_read").number(), 0); assert_eq!(receipt.get("member_read_calls").number(), 0);
        for key in ["archive_path_reopened", "live_source_reopened", "source_payload_embedded",
            "trail_attached", "overwritten", "truncated"] { assert!(!receipt.get(key).flag()); }
        assert!(receipt.get("accepted_trail_unchanged").flag()); assert!(receipt.get("accepted_query_unchanged").flag());
        assert!(receipt.get("complete_artifact").flag()); assert!(receipt.get("attach_eligible").flag());
        let bytes = fs::read(&destination).unwrap(); let trail = TrailView::open(&bytes, || false).unwrap();
        let mut expected = visits.to_vec();
        let mut title = "Original\0title";
        let encoded_edit = receipt.get("edit");
        match edit {
            TrailEdit::Rename { title: renamed } => {
                title = renamed; assert_eq!(encoded_edit.get("kind").text(), "rename");
                assert_eq!(encoded_edit.get("title").text(), renamed);
                assert_eq!(encoded_edit.get("ordinal"), &Json::Null);
            }
            TrailEdit::SetRationale { ordinal, rationale } => {
                expected[ordinal].rationale = rationale; assert_eq!(encoded_edit.get("kind").text(), "set-rationale");
                assert_eq!(encoded_edit.get("ordinal").number(), ordinal as u64);
                assert_eq!(encoded_edit.get("rationale").text(), rationale);
            }
            TrailEdit::Move { ordinal, destination } => {
                let moved = expected.remove(ordinal); expected.insert(destination, moved);
                assert_eq!(encoded_edit.get("kind").text(), "move");
                assert_eq!(encoded_edit.get("ordinal").number(), ordinal as u64);
                assert_eq!(encoded_edit.get("destination_ordinal").number(), destination as u64);
            }
            TrailEdit::Remove { ordinal } => {
                expected.remove(ordinal); assert_eq!(encoded_edit.get("kind").text(), "remove");
                assert_eq!(encoded_edit.get("ordinal").number(), ordinal as u64);
            }
        }
        if !matches!(edit, TrailEdit::Rename { .. }) { assert_eq!(encoded_edit.get("title"), &Json::Null); }
        if !matches!(edit, TrailEdit::SetRationale { .. }) { assert_eq!(encoded_edit.get("rationale"), &Json::Null); }
        if !matches!(edit, TrailEdit::Move { .. }) { assert_eq!(encoded_edit.get("destination_ordinal"), &Json::Null); }
        assert_eq!(trail.title(), title); assert_eq!(receipt.get("title").text(), title);
        assert_eq!(trail.parent_digest(), Some(digest)); assert_eq!(trail.len(), expected.len());
        assert_eq!(receipt.get("trail_digest").text(), trail.digest().to_hex());
        assert_eq!(receipt.get("total_items").number(), expected.len() as u64);
        let expected_bytes: u64 = expected.iter().map(|visit| visit.range.len().get()).sum();
        assert_eq!(receipt.get("reference_bytes").number(), expected_bytes);
        assert_eq!(trail.reference_bytes(), expected_bytes);
        assert_eq!(trail.entries().collect::<Result<Vec<_>, _>>().unwrap(), expected);
        assert_eq!(saved.trail_generation(), Some(9)); assert_eq!(saved.accepted_generation(), Some(7));
        assert_eq!(reads(&mut saved), before);
        assert_eq!(saved.trail_write_receipt(operation, || false).unwrap().as_str(), response.as_str());
        assert_eq!(reads(&mut saved), before);
    }
    let accepted = json(&saved.trail_page(9, 0, 32, || false).unwrap());
    assert_eq!(accepted.get("title").text(), "Original\0title");
    assert_eq!(accepted.get("total_items").number(), 4);
    assert_eq!(accepted.get("unavailable_items").number(), 2);
    assert_eq!(fs::read(f.root.join("original-parent.fcbt")).unwrap(), original);
    assert_eq!(fs::read(&parent_path).unwrap(), b"unrelated replacement at the old trail path");
}

#[test]
fn edit_rejects_stale_parent_or_invalid_visits_and_shares_the_write_operation_clock() {
    let f = Fixture::new(&[entry(b"a.rs", b"retained source")], true); let mut saved = f.open(63610);
    let first_path = f.root.join("first.fcbt");
    let first = saved.write_trail(request(&first_path, 10), || false).unwrap();
    saved.attach_trail(7, &first_path, || false).unwrap();
    let digest = Sha256Digest::from_hex(json(&first).get("trail_digest").text()).unwrap();
    let parent = SavedTrailWriteParent { generation: 7, digest };
    let destination = f.root.join("must-not-exist.fcbt");
    let base = SavedTrailEditRequest { operation: 11, parent, edit: TrailEdit::Rename { title: "Curated" }, destination: &destination };
    let before = reads(&mut saved); let title = "x".repeat(257); let rationale = "x".repeat(4097);
    for req in [
        SavedTrailEditRequest { parent: SavedTrailWriteParent { generation: 6, digest }, ..base },
        SavedTrailEditRequest { operation: 12, parent: SavedTrailWriteParent { digest: Sha256Digest::new([9; 32]), ..parent }, ..base },
        SavedTrailEditRequest { operation: 13, edit: TrailEdit::SetRationale { ordinal: 1, rationale: "Out of range" }, ..base },
        SavedTrailEditRequest { operation: 14, edit: TrailEdit::Move { ordinal: 0, destination: 1 }, ..base },
        SavedTrailEditRequest { operation: 15, edit: TrailEdit::Remove { ordinal: 1 }, ..base },
        SavedTrailEditRequest { operation: 16, edit: TrailEdit::Rename { title: &title }, ..base },
        SavedTrailEditRequest { operation: 17, edit: TrailEdit::SetRationale { ordinal: 0, rationale: &rationale }, ..base },
    ] {
        assert!(saved.edit_trail(req, || false).is_err()); assert!(!destination.exists());
        assert_eq!(reads(&mut saved), before);
        assert_eq!(saved.trail_write_receipt(10, || false).unwrap().as_str(), first.as_str());
    }
    assert!(saved.edit_trail(SavedTrailEditRequest { operation: 18, ..base }, || true).err().unwrap().is_canceled());
    assert_eq!(saved.last_trail_write_attempt(), 18); assert_eq!(saved.last_trail_write_operation(), Some(10));
    assert_eq!(saved.write_trail(request(&destination, 18), || false).err(), Some(Error::StaleWrite));
    assert_eq!(saved.trail_write_receipt(10, || false).unwrap().as_str(), first.as_str());
    let edited = saved.edit_trail(SavedTrailEditRequest { operation: 19, ..base }, || false).unwrap();
    assert_eq!(edited.exit_code(), EXIT_OK); assert_eq!(saved.last_trail_write_operation(), Some(19));
    assert_eq!(saved.trail_write_receipt(19, || false).unwrap().as_str(), edited.as_str());
    assert_eq!(saved.write_trail(request(&destination, 19), || false).err(), Some(Error::StaleWrite));
    assert_eq!(saved.trail_write_receipt(10, || false).err(), Some(Error::StaleWrite));
    assert_eq!(reads(&mut saved), before);
    let next_path = f.root.join("next-new.fcbt");
    assert_eq!(saved.write_trail(request(&next_path, 20), || false).unwrap().exit_code(), EXIT_OK);
    assert_eq!(saved.edit_trail(SavedTrailEditRequest { operation: 20, ..base }, || false).err(), Some(Error::StaleWrite));
    assert_eq!(saved.trail_generation(), Some(7));
}

#[test]
fn removing_the_last_visit_allows_empty_trail_rename_and_later_append() {
    let source = b"retained source"; let f = Fixture::new(&[entry(b"a.rs", source)], true);
    let mut saved = f.open(63620);
    let visit = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(source),
        source_length: source.len() as u64, path: b"a.rs", range: range(1, 4), rationale: "Only visit" };
    let (path, digest) = previous(&f, "One visit", &[visit]); saved.attach_trail(1, &path, || false).unwrap();
    let destination = f.root.join("empty.fcbt");
    let removed = saved.edit_trail(SavedTrailEditRequest { operation: 1,
        parent: SavedTrailWriteParent { generation: 1, digest }, edit: TrailEdit::Remove { ordinal: 0 },
        destination: &destination }, || false).unwrap();
    assert_eq!(removed.exit_code(), EXIT_OK);
    assert_eq!(json(&removed).get("total_items").number(), 0); assert_eq!(json(&removed).get("reference_bytes").number(), 0);
    assert_eq!(saved.trail_generation(), Some(1));
    let empty = fs::read(&destination).unwrap(); let empty = TrailView::open(&empty, || false).unwrap();
    assert!(empty.is_empty()); assert_eq!(empty.parent_digest(), Some(digest));
    saved.attach_trail(2, &destination, || false).unwrap();
    let renamed_path = f.root.join("renamed-empty.fcbt");
    let renamed = saved.edit_trail(SavedTrailEditRequest { operation: 2,
        parent: SavedTrailWriteParent { generation: 2, digest: empty.digest() },
        edit: TrailEdit::Rename { title: "New chapter" }, destination: &renamed_path }, || false).unwrap();
    assert_eq!(renamed.exit_code(), EXIT_OK); assert_eq!(json(&renamed).get("total_items").number(), 0);
    let renamed_digest = Sha256Digest::from_hex(json(&renamed).get("trail_digest").text()).unwrap();
    saved.attach_trail(3, &renamed_path, || false).unwrap();
    let final_path = f.root.join("next-chapter.fcbt");
    let appended = saved.write_trail(SavedTrailWriteRequest { operation: 3,
        parent: Some(SavedTrailWriteParent { generation: 3, digest: renamed_digest }), title: "New chapter",
        destination: &final_path, ..request(&final_path, 3) }, || false).unwrap();
    assert_eq!(appended.exit_code(), EXIT_OK);
    let final_bytes = fs::read(&final_path).unwrap(); let final_trail = TrailView::open(&final_bytes, || false).unwrap();
    assert_eq!(final_trail.len(), 1); assert_eq!(final_trail.title(), "New chapter");
    assert_eq!(final_trail.parent_digest(), Some(renamed_digest)); assert_eq!(final_trail.entry(0).unwrap().range, range(1, 4));
}

#[test]
fn edit_publication_refuses_overwrite_and_retains_canceled_effects_without_replay() {
    for (complete, session) in [(false, 63630), (true, 63631)] {
        let source = b"retained source"; let f = Fixture::new(&[entry(b"a.rs", source)], true); let mut saved = f.open(session);
        let visit = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(source),
            source_length: source.len() as u64, path: b"a.rs", range: range(1, 4), rationale: "Keep" };
        let (parent_path, digest) = previous(&f, "Original", &[visit]);
        let original = fs::read(&parent_path).unwrap(); saved.attach_trail(1, &parent_path, || false).unwrap();
        let parent = SavedTrailWriteParent { generation: 1, digest };
        let before = reads(&mut saved);
        let refused = saved.edit_trail(SavedTrailEditRequest { operation: 1, parent,
            edit: TrailEdit::Rename { title: "Cannot overwrite" }, destination: &parent_path }, || false).unwrap();
        assert_eq!(refused.exit_code(), EXIT_ERROR);
        assert_eq!(json(&refused).get("error").get("code").text(), "SAVED_TRAIL_DESTINATION_EXISTS");
        assert_eq!(json(&refused).get("effect").text(), "none"); assert!(!json(&refused).get("attach_eligible").flag());
        assert_eq!(fs::read(&parent_path).unwrap(), original);
        let destination = f.root.join("canceled.fcbt");
        let response = saved.edit_trail(SavedTrailEditRequest { operation: 2, parent,
            edit: TrailEdit::SetRationale { ordinal: 0, rationale: "Revised retained note" }, destination: &destination },
            || fs::metadata(&destination).is_ok_and(|m| !complete || m.len() > 0)).unwrap();
        assert_eq!(response.exit_code(), EXIT_CANCELED); let receipt = json(&response);
        assert_eq!(receipt.get("effect").text(), if complete { "complete-file-sync-unconfirmed" } else { "destination-created-incomplete" });
        assert_eq!(receipt.get("complete_artifact").flag(), complete); assert_eq!(receipt.get("attach_eligible").flag(), complete);
        assert_eq!(receipt.get("bytes_written").number(), if complete { receipt.get("trail_bytes").number() } else { 0 });
        assert!(!receipt.get("source_verified").flag()); assert!(receipt.get("source_anchors_unchanged").flag());
        fs::rename(&destination, f.root.join("preserved-effect.fcbt")).unwrap();
        fs::write(&destination, b"unrelated replacement").unwrap();
        assert_eq!(saved.trail_write_receipt(2, || false).unwrap().as_str(), response.as_str());
        assert_eq!(reads(&mut saved), before); assert_eq!(fs::read(&destination).unwrap(), b"unrelated replacement");
        assert_eq!(saved.trail_generation(), Some(1)); assert_eq!(fs::read(&parent_path).unwrap(), original);
    }
}

#[test]
fn editing_a_large_reference_only_trail_reports_partial_write_without_claiming_completion() {
    let f = Fixture::new(&[entry(b"a.rs", b"retained")], true); let mut saved = f.open(63640);
    let rationale = "r".repeat(4096);
    let visit = TrailEntry { archive: Sha256Digest::new([8; 32]), source: Sha256Digest::new([9; 32]),
        source_length: u64::MAX, path: b"unavailable.rs", range: range(u64::MAX - 3, u64::MAX),
        rationale: &rationale };
    let (path, digest) = previous(&f, "Foreign visits", &vec![visit; 20]); saved.attach_trail(1, &path, || false).unwrap();
    let before = reads(&mut saved); let destination = f.root.join("partial.fcbt");
    let response = saved.edit_trail(SavedTrailEditRequest { operation: 1,
        parent: SavedTrailWriteParent { generation: 1, digest },
        edit: TrailEdit::Move { ordinal: 0, destination: 19 }, destination: &destination },
        || fs::metadata(&destination).is_ok_and(|m| m.len() > 0)).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED); let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(receipt.get("bytes_written").number(), 64 * 1024);
    assert!(receipt.get("trail_bytes").number() > 64 * 1024);
    assert_eq!(receipt.get("total_items").number(), 20); assert_eq!(receipt.get("reference_bytes").number(), 60);
    assert!(!receipt.get("complete_artifact").flag()); assert!(!receipt.get("attach_eligible").flag());
    assert_eq!(saved.trail_generation(), Some(1));
    assert_eq!(saved.trail_write_receipt(1, || false).unwrap().as_str(), response.as_str());
    assert_eq!(reads(&mut saved), before);
}

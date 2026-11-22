#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production cursor + retained descriptor + bounded JSON + create-only effects.
//! Independent strict-RCH execution is required; these are authored regressions.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry, owner};
use support::{Json, parse};
use std::{fs, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{ResourceAllocationId, ResourceBudget, trail::{TrailBytes, TrailEntry}};
use fcb::store::{Sha256, Sha256Digest};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use fcb_app::host::{HostResponse, saved_repository::SavedRepositorySession};
use fcb_app::host::saved_repository::trail::{SavedTrailError, SavedTrailExportLimits,
    SavedTrailExportRequest, SavedTrailWriteParent};

const SOURCE: &[u8] = &[0xff, b'a', 0, b'\n', b'b', 0x80];
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn attach(f: &Fixture, saved: &mut SavedRepositorySession, mixed: bool) -> (PathBuf, SavedTrailWriteParent) {
    let item = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(SOURCE),
        source_length: SOURCE.len() as u64, path: b"src/raw-\xff.rs", range: range(1, 5), rationale: "é\0λ — exact note" };
    let mut entries = vec![item, item, TrailEntry { range: range(6, 6), rationale: "Point bookmark", ..item }];
    if mixed {
        entries.push(TrailEntry { archive: Sha256Digest::new([9; 32]), rationale: "Other archive visit", ..item });
        entries.push(TrailEntry { path: b"missing.rs", rationale: "Missing member visit", ..item });
    }
    let budget = ResourceBudget::new(owner(63700), ByteLength::new(16 * 1024 * 1024)).unwrap();
    let bytes = TrailBytes::encode(owner(63700), "Exact source journey", None, &entries,
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = f.root.join("tour.fcbt"); fs::write(&path, bytes.bytes()).unwrap();
    saved.attach_trail(7, &path, || false).unwrap();
    (path, SavedTrailWriteParent { generation: 7, digest: bytes.digest() })
}
fn request<'a>(path: &'a Path, trail: SavedTrailWriteParent, operation: u64) -> SavedTrailExportRequest<'a> {
    SavedTrailExportRequest { operation, trail, destination: path, limits: Default::default() }
}
fn read_bytes(saved: &mut SavedRepositorySession) -> u64 {
    json(&saved.info(|| false).unwrap()).get("member_bytes_read").number()
}

#[test]
fn retained_archive_export_preserves_exact_bytes_order_notes_and_unavailable_visits() {
    let f = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let mut saved = f.open(63701);
    let (path, trail) = attach(&f, &mut saved, true);
    fs::rename(&f.path, f.root.join("retained.fcbs")).unwrap();
    fs::write(&f.path, b"replacement is not the granted archive").unwrap();
    fs::rename(path, f.root.join("retained.fcbt")).unwrap();
    fs::write(f.root.join("tour.fcbt"), b"replacement trail").unwrap();
    let destination = f.root.join("source-pack.json");
    let response = saved.export_trail(request(&destination, trail, 1), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let receipt = json(&response);
    assert!(receipt.get("complete_artifact").flag());
    assert!(!receipt.get("coverage_complete").flag());
    assert_eq!(receipt.get("coverage").get("verified_entries").number(), 3);
    assert_eq!(receipt.get("coverage").get("unavailable_entries").number(), 2);
    assert_eq!(receipt.get("member_bytes_read").number(), 3 * SOURCE.len() as u64);
    let bytes = fs::read(&destination).unwrap(); let pack = parse(&bytes).unwrap();
    assert_eq!(pack.get("schema").text(), "fcb.trail-source-pack/1");
    assert_eq!(pack.get("source_representation").text(), "original-bytes-lowercase-hex");
    assert_eq!(pack.get("total_items").number(), 5);
    let rows = pack.get("entries").array(); assert_eq!(rows.len(), 5);
    for row in &rows[..2] {
        assert_eq!(row.get("selected_original_hex").text(), "61000a62");
        assert_eq!(row.get("selection").get("rationale").text(), "é\0λ — exact note");
        assert_eq!(row.get("selection").get("path").get("hex").text(), "7372632f7261772dff2e7273");
    }
    assert_eq!(rows[2].get("selected_original_hex").text(), "");
    for row in &rows[3..] {
        assert_eq!(row.get("readiness").text(), "unavailable");
        assert_eq!(row.get("selected_original_hex"), &Json::Null);
        assert!(!row.get("reason").text().is_empty());
    }
    assert_eq!(receipt.get("export_bytes").number(), bytes.len() as u64);
    assert_eq!(receipt.get("export_characters").number(), std::str::from_utf8(&bytes).unwrap().chars().count() as u64);
    assert_eq!(receipt.get("export_digest").text(), Sha256::digest(&bytes).to_hex());
    assert_eq!(saved.trail_export_receipt(1, || false).unwrap().as_str(), response.as_str());
    assert_eq!(saved.trail_generation(), Some(7));
}

#[test]
fn whole_selection_budgets_preserve_notes_and_metadata_refusal_performs_no_source_io() {
    let f = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let mut saved = f.open(63702); let (_, trail) = attach(&f, &mut saved, false);
    let destination = f.root.join("bounded.json");
    let req = SavedTrailExportRequest { limits: SavedTrailExportLimits { selected_bytes: 4, ..Default::default() },
        ..request(&destination, trail, 1) };
    let response = saved.export_trail(req, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let pack = parse(&fs::read(&destination).unwrap()).unwrap(); let rows = pack.get("entries").array();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get("selected_original_hex").text(), "61000a62");
    assert_eq!(rows[1].get("reason").text(), "TRAIL_EXPORT_SELECTED_BYTE_LIMIT");
    assert_eq!(rows[1].get("selected_original_hex"), &Json::Null);
    assert_eq!(rows[2].get("selected_original_hex").text(), "");
    assert_eq!(json(&response).get("member_bytes_read").number(), 2 * SOURCE.len() as u64);
    let refused = f.root.join("too-small.json"); let before = read_bytes(&mut saved);
    let result = saved.export_trail(SavedTrailExportRequest {
        limits: SavedTrailExportLimits { output_scalars: 1, ..Default::default() }, ..request(&refused, trail, 2)
    }, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_ERROR);
    assert_eq!(json(&result).get("effect").text(), "none");
    assert!(!refused.exists()); assert_eq!(read_bytes(&mut saved), before);
    assert_eq!(saved.trail_export_receipt(2, || false).unwrap().as_str(), result.as_str());
}

#[test]
fn cancellation_after_create_retains_exact_effect_without_replay_and_never_overwrites() {
    let f = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let mut saved = f.open(63703); let (_, trail) = attach(&f, &mut saved, false);
    let destination = f.root.join("canceled.json");
    let response = saved.export_trail(request(&destination, trail, 1), || destination.exists()).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(json(&response).get("effect").text(), "destination-created-incomplete");
    assert_eq!(json(&response).get("bytes_written").number(), 0);
    fs::write(&destination, b"external replacement after worker drained").unwrap();
    saved.detach_trail(8, || false).unwrap();
    assert_eq!(saved.trail_export_receipt(1, || false).unwrap().as_str(), response.as_str());
    assert_eq!(fs::read(&destination).unwrap(), b"external replacement after worker drained");
    assert!(matches!(saved.export_trail(request(&destination, trail, 1), || false), Err(SavedTrailError::StaleExport)));
    saved.attach_trail(9, &f.root.join("tour.fcbt"), || false).unwrap();
    let current = SavedTrailWriteParent { generation: 9, ..trail };
    let existing = saved.export_trail(request(&destination, current, 2), || false).unwrap();
    assert_eq!(existing.exit_code(), EXIT_ERROR);
    assert_eq!(json(&existing).get("error").get("code").text(), "SAVED_TRAIL_EXPORT_DESTINATION_EXISTS");
    assert_eq!(fs::read(&destination).unwrap(), b"external replacement after worker drained");
    let fresh = f.root.join("complete.json");
    let complete = saved.export_trail(request(&fresh, current, 3), || false).unwrap();
    assert_eq!(complete.exit_code(), EXIT_OK); assert!(json(&complete).get("coverage_complete").flag());
    let wrong = SavedTrailWriteParent { digest: Sha256Digest::new([7; 32]), ..current };
    let wrong_path = f.root.join("wrong.json");
    assert!(matches!(saved.export_trail(request(&wrong_path, wrong, 4), || false), Err(SavedTrailError::ParentMismatch)));
    assert!(!wrong_path.exists());
    assert_eq!(saved.trail_export_receipt(3, || false).unwrap().as_str(), complete.as_str());
}


const OTHER_SOURCE: &[u8] = &[0x80, b'B', 0xff, 0, b'z'];
const VALIDATION_BYTES: u64 = 256 * 1024 * 1024;
fn attach_two_archives(primary: &Fixture, other: &Fixture, saved: &mut SavedRepositorySession)
    -> SavedTrailWriteParent {
    let second_digest = other.open(63740).archive_digest();
    let first = TrailEntry { archive: saved.archive_digest(), source: Sha256::digest(SOURCE),
        source_length: SOURCE.len() as u64, path: b"src/raw-\xff.rs",
        range: range(1, 5), rationale: "Primary visit" };
    let second = TrailEntry { archive: second_digest, source: Sha256::digest(OTHER_SOURCE),
        source_length: OTHER_SOURCE.len() as u64, range: range(0, OTHER_SOURCE.len() as u64),
        rationale: "Supplemental é\0λ", ..first };
    let budget = ResourceBudget::new(owner(63741), ByteLength::new(16 * 1024 * 1024)).unwrap();
    let bytes = TrailBytes::encode(owner(63741), "Across explicit archives", None,
        &[first, second, second, first], &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = primary.root.join("mixed.fcbt"); fs::write(&path, bytes.bytes()).unwrap();
    saved.attach_trail(11, &path, || false).unwrap();
    SavedTrailWriteParent { generation: 11, digest: bytes.digest() }
}

#[test]
fn mixed_archive_export_routes_exact_visits_and_charges_duplicate_envelopes() {
    let primary = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let other = Fixture::new(&[entry(b"src/raw-\xff.rs", OTHER_SOURCE)], false);
    let mut saved = primary.open(63742);
    let trail = attach_two_archives(&primary, &other, &mut saved);
    let legacy_destination = primary.root.join("legacy-mixed.json");
    let legacy = saved.export_trail(request(&legacy_destination, trail, 1), || false).unwrap();
    assert_eq!(legacy.exit_code(), EXIT_PARTIAL);
    assert_eq!(json(&legacy).get("coverage").get("unavailable_entries").number(), 2);
    assert_eq!(json(&legacy).get("archive_scope").text(), "retained-single-archive");
    assert!(!legacy.as_str().contains("\"additional_archives\""));

    let other_copy = other.root.join("explicit-copy.fcbs"); fs::write(&other_copy, &other.bytes).unwrap();
    let primary_copy = primary.root.join("explicit-primary-copy.fcbs"); fs::write(&primary_copy, &primary.bytes).unwrap();
    fs::rename(&primary.path, primary.root.join("retained-primary.fcbs")).unwrap();
    fs::write(&primary.path, b"this replacement must never reopen").unwrap();
    let destination = primary.root.join("multi-source.json");
    let paths = [other.path.as_path(), other_copy.as_path(), primary_copy.as_path()];
    let response = saved.export_trail_with_archives(request(&destination, trail, 2),
        &paths, VALIDATION_BYTES, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let receipt = json(&response);
    assert_eq!(receipt.get("archive_scope").text(), "retained-primary-and-explicit-supplemental-archives");
    assert!(!receipt.get("archive_path_reopened").flag());
    assert!(receipt.get("coverage_complete").flag());
    assert_eq!(receipt.get("coverage").get("verified_entries").number(), 4);
    assert_eq!(receipt.get("member_bytes_read").number(), 2 * SOURCE.len() as u64 + 2 * OTHER_SOURCE.len() as u64);
    assert_eq!(receipt.get("member_bytes_read"), receipt.get("coverage").get("source_bytes_read"));
    let validation = receipt.get("archive_validation");
    for (key, expected) in [("supplied_archives", 4), ("validated_archives", 4),
        ("distinct_archives", 2), ("duplicate_archives", 2), ("opened_archive_paths", 3)] {
        assert_eq!(validation.get(key).number(), expected);
    }
    assert!(validation.get("validation_complete").flag());
    assert_eq!(validation.get("validation_bytes_admitted").number(),
        2 * (other.bytes.len() as u64 + 1) + primary.bytes.len() as u64 + 1);
    assert_eq!(validation.get("validation_bytes_read").number(),
        2 * other.bytes.len() as u64 + primary.bytes.len() as u64);
    assert!(validation.get("retained_directory_bytes").number() <= validation.get("peak_directory_bytes").number());
    assert!(validation.get("peak_directory_bytes").number() <= 64 * 1024 * 1024);
    let inventory = receipt.get("additional_archives").array();
    assert_eq!(inventory.len(), 3);
    for (ordinal, input) in inventory.iter().enumerate() {
        assert_eq!(input.get("ordinal").number(), ordinal as u64);
        assert!(input.get("opened").flag());
        assert_eq!(input.get("path").get("display").text(), paths[ordinal].to_str().unwrap());
    }
    assert_eq!(inventory[0].get("digest"), inventory[1].get("digest"));
    assert_eq!(inventory[2].get("digest"), receipt.get("archive_digest"));
    let bytes = fs::read(&destination).unwrap(); let pack = parse(&bytes).unwrap();
    assert_eq!(pack.get("archive"), &Json::Null);
    assert_eq!(pack.get("archives").array().len(), 2);
    assert_eq!(pack.get("archive_validation"), validation);
    let rows = pack.get("entries").array();
    assert_eq!(rows.len(), 4);
    for ordinal in [0, 3] { assert_eq!(rows[ordinal].get("selected_original_hex").text(), "61000a62"); }
    for ordinal in [1, 2] {
        assert_eq!(rows[ordinal].get("selected_original_hex").text(), "8042ff007a");
        assert_eq!(rows[ordinal].get("selection").get("rationale").text(), "Supplemental é\0λ");
    }
    assert_eq!(receipt.get("export_digest").text(), Sha256::digest(&bytes).to_hex());
    // Receipt recovery reads neither a supplemental path nor the destination.
    fs::rename(&other.path, other.root.join("moved-after-export.fcbs")).unwrap();
    fs::write(&other.path, b"unrelated later archive").unwrap();
    fs::write(&destination, b"external later destination").unwrap();
    assert_eq!(saved.trail_export_receipt(2, || false).unwrap().as_str(), response.as_str());
    assert_eq!(fs::read(&destination).unwrap(), b"external later destination");
}

#[test]
fn partial_supplemental_validation_retains_failure_work_without_member_io_or_output() {
    let primary = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let other = Fixture::new(&[entry(b"src/raw-\xff.rs", OTHER_SOURCE)], true);
    let mut saved = primary.open(63743);
    let trail = attach_two_archives(&primary, &other, &mut saved);
    let bad = other.root.join("corrupt.fcbs"); let mut corrupted = other.bytes.clone();
    *corrupted.last_mut().unwrap() ^= 1; fs::write(&bad, &corrupted).unwrap();
    let later = other.root.join("never-opened.fcbs");
    let destination = primary.root.join("refused-multi.json");
    let paths = [other.path.as_path(), bad.as_path(), later.as_path()];
    let before = read_bytes(&mut saved);
    let response = saved.export_trail_with_archives(request(&destination, trail, 1),
        &paths, VALIDATION_BYTES, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_ERROR);
    let receipt = json(&response); let validation = receipt.get("archive_validation");
    assert_eq!(receipt.get("error").get("code").text(), "SNAPSHOT_CHECKSUM_MISMATCH");
    assert_eq!(receipt.get("effect").text(), "none");
    for key in ["export_bytes", "export_characters", "bytes_written", "member_bytes_read", "member_read_calls"] {
        assert_eq!(receipt.get(key).number(), 0);
    }
    assert_eq!(receipt.get("coverage").get("entries_visited").number(), 0);
    assert!(!receipt.get("complete_artifact").flag()); assert!(!destination.exists());
    assert_eq!(read_bytes(&mut saved), before);
    assert!(!validation.get("validation_complete").flag());
    assert_eq!(validation.get("validated_archives").number(), 2);
    assert_eq!(validation.get("distinct_archives").number(), 2);
    assert_eq!(validation.get("opened_archive_paths").number(), 2);
    assert_eq!(validation.get("validation_bytes_read").number(), 2 * other.bytes.len() as u64);
    assert!(validation.get("validation_read_calls").number() > 0);
    let inputs = receipt.get("additional_archives").array();
    assert!(inputs[0].get("opened").flag()); assert_ne!(inputs[0].get("digest"), &Json::Null);
    assert!(inputs[1].get("opened").flag()); assert_eq!(inputs[1].get("digest"), &Json::Null);
    assert!(!inputs[2].get("opened").flag()); assert_eq!(inputs[2].get("digest"), &Json::Null);
    fs::write(&bad, &other.bytes).unwrap();
    assert_eq!(saved.trail_export_receipt(1, || false).unwrap().as_str(), response.as_str());

    let exhausted = primary.root.join("validation-budget.json");
    let limited = saved.export_trail_with_archives(request(&exhausted, trail, 2), &paths,
        other.bytes.len() as u64 + 1, || false).unwrap();
    let limited_receipt = json(&limited); let counts = limited_receipt.get("archive_validation");
    assert_eq!(limited_receipt.get("error").get("code").text(), "TRAIL_EXPORT_VALIDATION_BYTE_LIMIT");
    assert_eq!(counts.get("validated_archives").number(), 2);
    assert_eq!(counts.get("opened_archive_paths").number(), 2);
    assert_eq!(counts.get("validation_bytes_read").number(), other.bytes.len() as u64);
    assert_eq!(counts.get("validation_bytes_admitted").number(), other.bytes.len() as u64 + 1);
    assert!(!exhausted.exists()); assert_eq!(read_bytes(&mut saved), before);
}

#[test]
fn mixed_archive_member_and_selection_budgets_are_global_across_repeated_visits() {
    let primary = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let other = Fixture::new(&[entry(b"src/raw-\xff.rs", OTHER_SOURCE)], true);
    let mut saved = primary.open(63744);
    let trail = attach_two_archives(&primary, &other, &mut saved);
    let paths = [other.path.as_path()];
    for (operation, limits, omitted_key) in [
        (1, SavedTrailExportLimits { verification_bytes: SOURCE.len() as u64, ..Default::default() }, "omitted_source_limit"),
        (2, SavedTrailExportLimits { selected_bytes: 4, ..Default::default() }, "omitted_output_limit"),
        (3, SavedTrailExportLimits { entries: 0, ..Default::default() }, "omitted_entry_limit"),
    ] {
        let destination = primary.root.join(format!("global-budget-{operation}.json"));
        let response = saved.export_trail_with_archives(SavedTrailExportRequest {
            limits, ..request(&destination, trail, operation) }, &paths, VALIDATION_BYTES, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let receipt = json(&response); let coverage = receipt.get("coverage");
        assert_eq!(coverage.get(omitted_key).number(), if operation == 3 { 4 } else { 3 });
        assert_eq!(coverage.get("verified_entries").number(), if operation == 3 { 0 } else { 1 });
        assert_eq!(receipt.get("member_bytes_read").number(), if operation == 3 { 0 } else { SOURCE.len() as u64 });
        assert!(receipt.get("archive_validation").get("validation_complete").flag());
        assert!(receipt.get("complete_artifact").flag()); assert!(!receipt.get("coverage_complete").flag());
    }
}

#[test]
fn supplemental_export_cancellation_after_create_retains_effect_and_limit_preflight_does_no_io() {
    let primary = Fixture::new(&[entry(b"src/raw-\xff.rs", SOURCE)], true);
    let other = Fixture::new(&[entry(b"src/raw-\xff.rs", OTHER_SOURCE)], true);
    let mut saved = primary.open(63745);
    let trail = attach_two_archives(&primary, &other, &mut saved);
    let destination = primary.root.join("canceled-multi.json");
    let response = saved.export_trail_with_archives(request(&destination, trail, 1), &[other.path.as_path()],
        VALIDATION_BYTES, || destination.exists()).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert!(receipt.get("archive_validation").get("validation_complete").flag());
    assert_eq!(receipt.get("bytes_written").number(), 0);
    fs::write(&destination, b"external replacement").unwrap();
    fs::rename(&other.path, other.root.join("moved.fcbs")).unwrap();
    assert_eq!(saved.trail_export_receipt(1, || false).unwrap().as_str(), response.as_str());
    assert_eq!(fs::read(&destination).unwrap(), b"external replacement");
    let missing = other.root.join("never-authorized.fcbs"); let refused = primary.root.join("preflight.json");
    let before = read_bytes(&mut saved);
    assert!(matches!(saved.export_trail_with_archives(request(&refused, trail, 2), &[missing.as_path(); 8],
        VALIDATION_BYTES, || false), Err(SavedTrailError::InvalidLimits)));
    assert!(matches!(saved.export_trail_with_archives(request(&refused, trail, 3), &[missing.as_path()],
        VALIDATION_BYTES + 1, || false), Err(SavedTrailError::InvalidLimits)));
    assert!(!refused.exists()); assert_eq!(read_bytes(&mut saved), before);
    assert_eq!(saved.trail_export_receipt(1, || false).unwrap().as_str(), response.as_str());
}

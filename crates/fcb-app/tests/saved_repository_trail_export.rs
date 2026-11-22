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

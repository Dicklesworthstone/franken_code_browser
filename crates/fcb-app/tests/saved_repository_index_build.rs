#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production builder/publication/retained-session regressions. Authored
//! code-first; execution belongs to independent batch verification.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{ffi::OsString, fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write},
    os::unix::{ffi::{OsStrExt, OsStringExt}, fs::PermissionsExt}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession, SavedRepositoryError,
    SavedIndexBuildLimits, Sha256Digest, index_build::{MAX_INDEX_BUILD_FILE_BYTES,
        MAX_INDEX_BUILD_SOURCE_BYTES, MAX_INDEX_BUILD_GRAMS}}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn pin(value: &Json) -> Sha256Digest { Sha256Digest::from_hex(value.get("index_pin").text()).unwrap() }
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let info = json(&saved.info(|| false).unwrap());
    (info.get("member_bytes_read").number(), info.get("member_read_calls").number())
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

#[test]
fn production_build_returns_trusted_pin_without_changing_queries_or_attaching_itself() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle"), entry(b"b.rs", b"unrelated")], true);
    let mut saved = f.open(61001);
    saved.search(7, "needle", 8, || false).unwrap();
    let (reader, _) = saved.open_hit_reader(owner(61002), 7, 1, || false).unwrap();
    let destination = f.root.join("created.fcbd");
    let response = saved.build_index(41, &destination, SavedIndexBuildLimits::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let receipt = json(&response);
    assert_eq!(receipt.get("command").text(), "index-build");
    assert_eq!(receipt.get("operation").number(), 41);
    assert_eq!(receipt.get("last_index_build_attempt").number(), 41);
    assert_eq!(receipt.get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(receipt.get("index_kind").text(), "demand-paged-postings-v1");
    assert_eq!(receipt.get("index_pin_scope").text(), "page-manifest-envelope-sha256");
    assert!(receipt.get("coverage_complete").flag());
    assert!(receipt.get("attach_eligible").flag());
    assert!(!receipt.get("index_attached").flag());
    assert!(!receipt.get("power_loss_qualified").flag());
    assert!(receipt.get("source_derived_sensitive").flag());
    assert_eq!(receipt.get("index_indexed_files").number(), 2);
    assert_eq!(receipt.get("index_uncovered_files").number(), 0);
    assert_eq!(receipt.get("member_bytes_read").number(), 15);
    assert_eq!(receipt.get("bytes_written"), receipt.get("index_bytes"));
    assert_eq!(fs::metadata(&destination).unwrap().len(), receipt.get("index_bytes").number());
    assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o077, 0);
    assert_eq!(saved.index_generation(), None);
    assert_eq!(saved.accepted_generation(), Some(7));
    assert_eq!(json(&saved.results(7, 0, 8, || false).unwrap()).get("retained_hits").number(), 1);
    assert_eq!(reader.capture().bytes(), b"needle");
    let before = reads(&mut saved);
    assert_eq!(saved.index_build_receipt(41, || false).unwrap().as_str(), response.as_str());
    assert_eq!(reads(&mut saved), before);
    saved.attach_index(1, &destination, pin(&receipt), || false).unwrap();
    let searched = json(&saved.search(8, "needle", 8, || false).unwrap());
    assert_eq!(searched.get("retained_hits").number(), 1);
    assert_eq!(searched.get("index_eliminated_files").number(), 1);
    assert_eq!(searched.get("member_bytes_read").number(), 6);
    assert_eq!(saved.index_build_receipt(41, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn replacing_the_archive_path_cannot_substitute_new_source_for_an_open_build() {
    let f = Fixture::new(&[entry(b"same.rs", b"original needle")], true);
    let other = Fixture::new(&[entry(b"same.rs", b"replacement text")], true);
    let mut saved = f.open(61010);
    let original_digest = saved.archive_digest();
    fs::rename(&f.path, f.root.join("held.fcbs")).unwrap();
    fs::write(&f.path, &other.bytes).unwrap();
    let destination = f.root.join(OsString::from_vec(b"index-\xff.fcbd".to_vec()));
    let receipt = json(&saved.build_index(1, &destination, Default::default(), || false).unwrap());
    assert_eq!(receipt.get("archive_digest").text(), original_digest.to_hex());
    assert_eq!(receipt.get("destination").get("encoding").text(), "unix-bytes");
    assert_eq!(receipt.get("destination").get("hex").text(), hex(destination.as_os_str().as_bytes()));
    assert_eq!(receipt.get("member_bytes_read").number(), 15);
    saved.attach_index(1, &destination, pin(&receipt), || false).unwrap();
    assert_eq!(json(&saved.search(1, "original", 8, || false).unwrap()).get("retained_hits").number(), 1);
    assert_eq!(json(&saved.search(2, "replacement", 8, || false).unwrap()).get("retained_hits").number(), 0);
    let mut replacement_session = f.open(61011);
    assert!(replacement_session.attach_index(1, &destination, pin(&receipt), || false).is_err());
}

#[test]
fn quota_refusals_missing_members_and_discovery_limits_remain_visible_and_searchable() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"another needle"),
        SnapshotEntry { path: b"c", observed_bytes: 99, data: SnapshotData::Unavailable("DENIED") }], false);
    let mut saved = f.open(61020); let destination = f.root.join("partial.fcbd");
    let limits = SavedIndexBuildLimits { max_file_bytes: 6, max_source_bytes: 6, ..Default::default() };
    let response = saved.build_index(1, &destination, limits, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let receipt = json(&response);
    assert_eq!(receipt.get("status").text(), "ok");
    assert_eq!(receipt.get("index_members").number(), 3);
    assert_eq!(receipt.get("index_indexed_files").number(), 1);
    assert_eq!(receipt.get("index_uncovered_files").number(), 1);
    assert_eq!(receipt.get("index_unavailable_files").number(), 1);
    assert_eq!(receipt.get("member_bytes_read").number(), 6);
    assert!(!receipt.get("coverage_complete").flag());
    assert!(receipt.get("attach_eligible").flag());
    saved.attach_index(1, &destination, pin(&receipt), || false).unwrap();
    let searched = saved.search(1, "needle", 8, || false).unwrap();
    assert_eq!(searched.exit_code(), EXIT_PARTIAL);
    assert_eq!(json(&searched).get("retained_hits").number(), 2);
}

#[test]
fn aggregate_byte_and_zero_gram_limits_never_create_false_negative_certificates() {
    for (number, limits, expected_reads, uncovered) in [
        (0u64, SavedIndexBuildLimits { max_source_bytes: 6, ..Default::default() }, 6u64, 1u64),
        (1, SavedIndexBuildLimits { max_grams: 0, ..Default::default() }, 0, 2),
        (2, SavedIndexBuildLimits { max_file_bytes: 0, max_source_bytes: 0, max_grams: 0 }, 0, 2),
    ] {
        let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"needle")], true);
        let mut saved = f.open(61030 + number); let destination = f.root.join("bounded.fcbd");
        let response = saved.build_index(1, &destination, limits, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let receipt = json(&response);
        assert_eq!(receipt.get("member_bytes_read").number(), expected_reads);
        assert_eq!(receipt.get("index_uncovered_files").number(), uncovered);
        saved.attach_index(1, &destination, pin(&receipt), || false).unwrap();
        assert_eq!(json(&saved.search(1, "needle", 8, || false).unwrap()).get("retained_hits").number(), 2);
    }
}

#[test]
fn old_index_and_query_survive_success_then_destination_refusal_and_invalid_attempts() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true);
    let (old_path, old_pin) = f.index(); let old_bytes = fs::read(&old_path).unwrap();
    let mut saved = f.open(61040);
    saved.attach_index(9, &old_path, old_pin, || false).unwrap();
    saved.search(4, "needle", 8, || false).unwrap();
    let destination = f.root.join("new.fcbd");
    saved.build_index(1, &destination, Default::default(), || false).unwrap();
    assert_eq!(saved.index_generation(), Some(9)); assert_eq!(saved.accepted_generation(), Some(4));
    let refused = saved.build_index(2, &old_path, Default::default(), || false).unwrap();
    assert_eq!(refused.exit_code(), EXIT_ERROR);
    let receipt = json(&refused);
    assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("bytes_written").number(), 0);
    assert_eq!(receipt.get("error").get("code").text(), "SAVED_INDEX_DESTINATION_EXISTS");
    assert!(!receipt.get("attach_eligible").flag());
    assert_eq!(fs::read(&old_path).unwrap(), old_bytes);
    for (operation, limits) in [
        (3, SavedIndexBuildLimits { max_file_bytes: MAX_INDEX_BUILD_FILE_BYTES + 1, ..Default::default() }),
        (4, SavedIndexBuildLimits { max_source_bytes: MAX_INDEX_BUILD_SOURCE_BYTES + 1, ..Default::default() }),
        (5, SavedIndexBuildLimits { max_grams: MAX_INDEX_BUILD_GRAMS + 1, ..Default::default() }),
    ] {
        let path = f.root.join(format!("invalid-{operation}.fcbd"));
        assert_eq!(saved.build_index(operation, &path, limits, || false).err(), Some(SavedRepositoryError::InvalidLimits));
        assert!(!path.exists());
        assert_eq!(saved.index_build_receipt(2, || false).unwrap().as_str(), refused.as_str());
        assert_eq!(saved.index_generation(), Some(9)); assert_eq!(saved.accepted_generation(), Some(4));
    }
    let info = json(&saved.info(|| false).unwrap());
    assert_eq!(info.get("last_index_build_attempt").number(), 5);
    assert_eq!(info.get("last_index_build_operation").number(), 2);
    assert_eq!(json(&saved.results(4, 0, 8, || false).unwrap()).get("retained_hits").number(), 1);
}

#[test]
fn canceled_before_admission_preserves_the_previous_receipt_and_consumes_the_attempt() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let mut saved = f.open(61050);
    assert_eq!(saved.index_build_receipt(1, || false).err(), Some(SavedRepositoryError::MissingIndexBuild));
    let first = saved.build_index(1, &f.root.join("first.fcbd"), Default::default(), || false).unwrap();
    let destination = f.root.join("canceled.fcbd");
    assert_eq!(saved.build_index(2, &destination, Default::default(), || true).err(), Some(SavedRepositoryError::Canceled));
    assert!(!destination.exists());
    assert_eq!(saved.last_index_build_attempt(), 2); assert_eq!(saved.last_index_build_operation(), Some(1));
    assert_eq!(saved.index_build_receipt(1, || false).unwrap().as_str(), first.as_str());
    assert_eq!(saved.index_build_receipt(2, || false).err(), Some(SavedRepositoryError::StaleIndexBuild));
    assert_eq!(saved.build_index(2, &destination, Default::default(), || false).err(), Some(SavedRepositoryError::StaleIndexBuild));
    assert!(!destination.exists());
}

#[test]
fn cancellation_during_the_engine_is_a_retained_terminal_no_effect_receipt() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let mut saved = f.open(61060);
    let destination = f.root.join("engine-canceled.fcbd"); let mut polls = 0;
    let response = saved.build_index(1, &destination, Default::default(), || { polls += 1; polls == 2 }).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    let receipt = json(&response);
    assert_eq!(receipt.get("status").text(), "error");
    assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("index_members"), &Json::Null);
    assert_eq!(receipt.get("index_pin"), &Json::Null);
    assert_eq!(receipt.get("index_bytes").number(), 0);
    assert_eq!(receipt.get("member_bytes_read").number(), 0);
    assert!(!destination.exists());
    assert_eq!(saved.index_build_receipt(1, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn canceled_after_creation_keeps_its_receipt_even_if_the_output_path_is_later_replaced() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let mut saved = f.open(61070);
    saved.search(1, "needle", 8, || false).unwrap();
    let destination = f.root.join("created-empty.fcbd");
    let response = saved.build_index(17, &destination, Default::default(), || destination.exists()).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(receipt.get("bytes_written").number(), 0);
    assert!(!receipt.get("attach_eligible").flag());
    assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    let before = reads(&mut saved);
    fs::rename(&destination, f.root.join("incomplete-preserved.fcbd")).unwrap();
    fs::write(&destination, b"another file now occupies this name").unwrap();
    drop(response); // Model a lost reply: only the session's operation remains.
    let recovered = saved.index_build_receipt(17, || false).unwrap();
    assert_eq!(json(&recovered), receipt);
    assert_eq!(reads(&mut saved), before);
    assert_eq!(fs::read(&destination).unwrap(), b"another file now occupies this name");
    assert_eq!(saved.build_index(17, &destination, Default::default(), || false).err(), Some(SavedRepositoryError::StaleIndexBuild));
    assert_eq!(saved.accepted_generation(), Some(1));
}

#[test]
fn cancellation_after_complete_write_returns_an_explicitly_attachable_unsynced_artifact() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true); let mut saved = f.open(61080);
    let destination = f.root.join("written.fcbd");
    let response = saved.build_index(1, &destination, Default::default(),
        || fs::metadata(&destination).is_ok_and(|m| m.len() > 0)).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "complete-file-sync-unconfirmed");
    assert_eq!(receipt.get("bytes_written"), receipt.get("index_bytes"));
    assert!(receipt.get("attach_eligible").flag());
    assert_eq!(saved.index_generation(), None);
    saved.attach_index(1, &destination, pin(&receipt), || false).unwrap();
    assert_eq!(json(&saved.search(1, "needle", 8, || false).unwrap()).get("retained_hits").number(), 1);
    assert_eq!(saved.index_build_receipt(1, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn partial_chunk_write_is_never_reported_as_complete_or_attachable() {
    let mut source = Vec::new(); let mut state = 0x7ed5_51abu32;
    for _ in 0..20_000 { state ^= state << 13; state ^= state >> 17; state ^= state << 5; source.push(state as u8); }
    let f = Fixture::new(&[entry(b"random.bin", &source)], true); let mut saved = f.open(61090);
    let destination = f.root.join("partial-write.fcbd");
    let response = saved.build_index(1, &destination, Default::default(),
        || fs::metadata(&destination).is_ok_and(|m| m.len() > 0)).unwrap();
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    let receipt = json(&response);
    assert_eq!(receipt.get("effect").text(), "destination-created-incomplete");
    assert_eq!(receipt.get("bytes_written").number(), 64 * 1024);
    assert!(receipt.get("index_bytes").number() > receipt.get("bytes_written").number());
    assert!(!receipt.get("attach_eligible").flag());
    assert_eq!(fs::metadata(&destination).unwrap().len(), 64 * 1024);
    assert!(saved.attach_index(1, &destination, pin(&receipt), || false).is_err());
    assert_eq!(saved.index_build_receipt(1, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn in_place_source_damage_refuses_build_but_preserves_previously_accepted_results_and_index() {
    let source = b"unique retained needle source";
    let f = Fixture::new(&[entry(b"a", source)], true); let (old_path, old_pin) = f.index();
    let mut saved = f.open(61100); saved.attach_index(4, &old_path, old_pin, || false).unwrap();
    saved.search(3, "needle", 8, || false).unwrap();
    let (reader, _) = saved.open_hit_reader(owner(61101), 3, 1, || false).unwrap();
    let at = f.bytes.windows(source.len()).position(|b| b == source).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    let destination = f.root.join("refused.fcbd");
    let response = saved.build_index(1, &destination, Default::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_ERROR);
    let receipt = json(&response);
    assert_eq!(receipt.get("error").get("code").text(), "SNAPSHOT_MEMBER_CHANGED");
    assert_eq!(receipt.get("effect").text(), "none");
    assert_eq!(receipt.get("index_pin"), &Json::Null);
    assert!(!destination.exists());
    assert_eq!(saved.index_generation(), Some(4)); assert_eq!(saved.accepted_generation(), Some(3));
    assert_eq!(json(&saved.results(3, 0, 8, || false).unwrap()).get("retained_hits").number(), 1);
    assert_eq!(reader.capture().bytes(), source);
    assert_eq!(saved.index_build_receipt(1, || false).unwrap().as_str(), response.as_str());
}

#[test]
fn receipt_lookup_is_session_owned_and_a_new_terminal_operation_retires_only_the_old_receipt() {
    let f = Fixture::new(&[entry(b"a", b"needle")], true);
    let mut first = f.open(61110); let mut second = f.open(61111);
    let response = first.build_index(1, &f.root.join("first.fcbd"), Default::default(), || false).unwrap();
    assert_eq!(second.index_build_receipt(1, || false).err(), Some(SavedRepositoryError::MissingIndexBuild));
    first.build_index(2, &f.root.join("second.fcbd"), Default::default(), || false).unwrap();
    assert_eq!(first.index_build_receipt(1, || false).err(), Some(SavedRepositoryError::StaleIndexBuild));
    let latest = first.index_build_receipt(2, || false).unwrap();
    drop(first);
    assert_eq!(json(&response).get("operation").number(), 1);
    assert_eq!(json(&latest).get("operation").number(), 2);
    assert_eq!(fs::metadata(f.root.join("first.fcbd")).unwrap().len(), json(&response).get("bytes_written").number());
}

#[test]
fn every_observed_construction_and_publication_cancel_keeps_the_prior_index_and_query() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"another needle")], true);
    let (old_path, old_pin) = f.index(); let mut saved = f.open(61120);
    saved.attach_index(6, &old_path, old_pin, || false).unwrap();
    saved.search(5, "needle", 8, || false).unwrap();
    let mut polls = 0;
    saved.build_index(1, &f.root.join("baseline.fcbd"), Default::default(), || { polls += 1; false }).unwrap();
    let mut canceled_after_build = false;
    let mut canceled_before_create = false;
    let mut canceled_after_file_sync = false;
    // Poll 1 is before admission and has its own ordinary-error regression.
    for stop_at in 2..=polls {
        let operation = stop_at as u64;
        let destination = f.root.join(format!("stop-{stop_at}.fcbd")); let mut calls = 0;
        let response = saved.build_index(operation, &destination, Default::default(),
            || { calls += 1; calls == stop_at }).unwrap();
        assert_eq!(response.exit_code(), EXIT_CANCELED, "checkpoint {stop_at}");
        let receipt = json(&response);
        canceled_after_build |= receipt.get("index_members") != &Json::Null && receipt.get("index_pin") == &Json::Null;
        canceled_before_create |= receipt.get("index_pin") != &Json::Null && receipt.get("effect").text() == "none";
        canceled_after_file_sync |= receipt.get("effect").text() == "complete-file-sync-requested";
        assert_eq!(saved.index_generation(), Some(6)); assert_eq!(saved.accepted_generation(), Some(5));
        assert_eq!(json(&saved.results(5, 0, 8, || false).unwrap()).get("retained_hits").number(), 2);
        assert_eq!(saved.index_build_receipt(operation, || false).unwrap().as_str(), response.as_str());
        match receipt.get("effect").text() {
            "none" => assert!(!destination.exists()),
            _ => assert_eq!(fs::metadata(&destination).unwrap().len(), receipt.get("bytes_written").number()),
        }
    }
    assert!(canceled_after_build, "covers inversion/encoding cancellation after segment construction");
    assert!(canceled_before_create, "covers cancellation after trusted pin production but before create");
    assert!(canceled_after_file_sync, "covers a complete artifact whose parent sync was canceled");
}

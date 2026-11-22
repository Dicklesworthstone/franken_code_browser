#![forbid(unsafe_code)]

//! A restored legal desk must remain readable after JSON escaping. Observed
//! paths preserve both their escaped label and their lossless byte spelling;
//! personal note bodies can contain NUL without truncating an FFI response.

mod support;

use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ObservedExtent,
    ResourceAllocationId, ResourceBudget};
use fcb::ui::reading_panes::{MAX_READING_PATH_BYTES,
    desk::{ReadingDesk, DeskLimits, MAX_ANNOTATION_BODY_BYTES}};
use fcb_app::{EXIT_OK, host::desk::DeskSession};
use support::parse;

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget(owner: ArenaOwnerId) -> ResourceBudget {
    ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap()
}

fn round_trip_state(count: usize, previous_limit: usize) {
    let source_owner = owner(89600 + count as u64);
    let desk_owner = owner(89700 + count as u64);
    let restored_owner = owner(89800 + count as u64);
    let source_budget = budget(source_owner);
    let desk_budget = budget(desk_owner);
    let limits = DeskLimits { sources: count, bookmarks: count,
        source_bytes: 1024, retained_bytes: 64 * 1024, ..Default::default() };
    let mut desk = ReadingDesk::new(desk_owner, limits, &desk_budget, allocation(1)).unwrap();
    let selected = ByteRange::new(ByteOffset::new(0), ByteOffset::new(6)).unwrap();
    // These are provenance labels, not paths to open. Each invalid byte expands
    // to four display characters, five JSON bytes, plus two path_hex digits.
    let path = vec![0xff; MAX_READING_PATH_BYTES];
    let body = "\0".repeat(MAX_ANNOTATION_BODY_BYTES);
    for index in 0..count {
        let identity = index as u64 + 1;
        let request = CaptureRequest::new(FileId::new(source_owner, identity).unwrap(),
            SourceRevision::new(source_owner, identity).unwrap()).unwrap().with_range(selected).unwrap();
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(6), b"needle",
            &source_budget, allocation(identity)).unwrap();
        let changed = desk.import_observation(desk.revision(), desk.last_attempt() + 1,
            &extent, &path, DetectedEncoding::Utf8 { has_bom: false }, selected,
            identity, allocation(100 + identity), || false).unwrap();
        desk.edit_observed_passage(desk.revision(), desk.last_attempt() + 1,
            changed.passage, 1, &format!("Evidence {index}"), &body, || false).unwrap();
    }
    assert_eq!(desk.retained_source_count(), count);
    let checkpoint = desk.checkpoint(desk.revision(), allocation(1000), || false).unwrap();
    let mut restored = DeskSession::new(restored_owner, limits).unwrap();
    restored.restore_checkpoint_bytes(0, 1, checkpoint.bytes(), || false).unwrap();
    let response = restored.state(|| false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    assert!(response.as_str().len() > previous_limit, "fixture must exceed the former framing limit");
    assert!(response.as_str().len() <= 16 * 1024 * 1024);
    let state = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(state.get("command").text(), "state");
    assert_eq!(state.get("retained_sources").number(), count as u64);
    assert_eq!(state.get("retained_source_bytes").number(), count as u64 * 6);
    assert_eq!(state.get("observations").array().len(), count);
    assert_eq!(state.get("observed_passages").array().len(), count);
    assert!(state.get("panes").array().is_empty());
    assert!(state.get("bookmarks").array().is_empty());
    let path_hex = "ff".repeat(MAX_READING_PATH_BYTES);
    for (index, observation) in state.get("observations").array().iter().enumerate() {
        assert_eq!(observation.get("origin_owner").number(), source_owner.get());
        assert_eq!(observation.get("origin_file").number(), index as u64 + 1);
        assert_eq!(observation.get("path_hex").text(), path_hex);
        assert_eq!(observation.get("path").text().len(), 4 * MAX_READING_PATH_BYTES);
    }
    for (index, passage) in state.get("observed_passages").array().iter().enumerate() {
        assert_eq!(passage.get("label").text(), format!("Evidence {index}"));
        assert_eq!(passage.get("body").text(), body);
        assert_eq!(passage.get("update_sequence").number(), 2);
        assert_eq!(passage.get("selection").get("start").number(), 0);
        assert_eq!(passage.get("selection").get("end").number(), 6);
    }
}

#[test]
fn maximum_legal_observed_state_survives_checkpoint_restore_and_exceeds_eight_mib() {
    round_trip_state(64, 8 * 1024 * 1024);
}

#[test]
fn default_observed_count_preserves_escaped_state_larger_than_four_mib() {
    round_trip_state(32, 4 * 1024 * 1024);
}

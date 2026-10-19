#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;

use std::{path::Path, os::unix::ffi::OsStrExt};
use fcb::{ArenaOwnerId, analysis::comparison::{CaptureComparison, ComparisonError},
    search::{QueryGeneration, ResourceAllocationId, ResourceBudget}, ByteLength, store::Sha256};
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::reader::{ReaderSession, ReaderSessionError, ReaderComparisonLimits}};
use support::{Json, parse};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn reader(id: u64, bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(id), Path::new("same-label.rs"), bytes, || false).unwrap()
}
fn ranges(document: &Json, before: &[u8], after: &[u8]) {
    let mut previous = [0, 0];
    let mut edits = 0;
    for (index, span) in document.get("spans").array().iter().enumerate() {
        assert_eq!(span.get("span").number(), index as u64);
        let a = span.get("before_range"); let b = span.get("after_range");
        let starts = [a.get("start").number(), b.get("start").number()];
        let ends = [a.get("end").number(), b.get("end").number()];
        assert_eq!(starts, previous);
        assert!(starts[0] <= ends[0] && ends[0] <= before.len() as u64);
        assert!(starts[1] <= ends[1] && ends[1] <= after.len() as u64);
        assert_ne!(starts, ends);
        if span.get("kind").text() == "equal" {
            assert_eq!(&before[starts[0] as usize..ends[0] as usize],
                &after[starts[1] as usize..ends[1] as usize]);
        } else if span.get("kind").text() == "changed" {
            edits += ends[0] - starts[0] + ends[1] - starts[1];
        } else { assert_eq!(span.get("kind").text(), "unresolved"); }
        previous = ends;
    }
    assert_eq!(previous, [before.len() as u64, after.len() as u64]);
    if document.get("comparison_complete").flag() {
        assert_eq!(document.get("edit_distance").number(), edits);
    }
}

#[test]
fn explicit_pair_transfer_keeps_original_authority_and_existing_engine_owner_checks() {
    let mut before = reader(9701, b"same before same");
    let after = reader(9702, b"same after same");
    before.search(7, "before", 10, 4096, || false).unwrap();
    let budget = ResourceBudget::new(owner(9701), ByteLength::new(8 * 1024 * 1024)).unwrap();
    assert_eq!(CaptureComparison::build(before.capture(), after.capture(),
        QueryGeneration::new(owner(9701), 1).unwrap(), ReaderComparisonLimits::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).err(), Some(ComparisonError::OwnerMismatch));
    let response = before.compare_retained(&after, owner(9703), 1,
        ReaderComparisonLimits::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let document = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(document.get("owner").number(), 9701);
    assert_eq!(document.get("comparison_owner").number(), 9703);
    assert_eq!(document.get("before_source").get("owner").number(), 9701);
    assert_eq!(document.get("after_source").get("owner").number(), 9702);
    for (side, bytes) in [("before_source", before.capture().bytes()), ("after_source", after.capture().bytes())] {
        assert_eq!(document.get(side).get("capture_sha256").text(), Sha256::digest(bytes).to_hex());
    }
    assert_eq!(before.accepted_generation(), Some(7));
    assert!(before.copy_hit(7, 0, || false).unwrap().as_str().contains("6265666f7265"));
    assert_eq!(after.accepted_generation(), None);
    assert!(!document.get("source_reopened").flag());
    assert_eq!(document.get("additional_source_bytes_read").number(), 0);
    ranges(&document, before.capture().bytes(), after.capture().bytes());
}

#[test]
fn exact_partitions_cover_insertions_empty_sources_and_original_encoding_bytes() {
    let cases: &[(&[u8], &[u8])] = &[
        (b"", b""), (b"", b"insert"), (b"delete", b""),
        (b"\xff\0a\r\n", b"\xfe\0a\r\n"),
        (b"\xff\xfea\0", b"\xfe\xff\0a"),
        ("caf\u{e9}".as_bytes(), "cafe\u{301}".as_bytes()),
        (b"repeated repeated repeated", b"repeated changed repeated"),
    ];
    for (index, &(a, b)) in cases.iter().enumerate() {
        let first = 9710 + index as u64 * 3;
        let mut before = reader(first, a); let after = reader(first + 1, b);
        let response = before.compare_retained(&after, owner(first + 2), 1,
            ReaderComparisonLimits::default(), || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let document = parse(response.as_str().as_bytes()).unwrap();
        assert_eq!(document.get("relation").text(), if a == b { "identical" } else { "different" });
        ranges(&document, a, b);
    }
}

#[test]
fn work_and_edit_limits_return_unresolved_interiors_without_false_equality() {
    let a = b"prefix aaaaaaaaaaaaaaaaaa suffix";
    let b = b"prefix bbbbbbbbbbbbbbbbbb suffix";
    let mut before = reader(9761, a); let after = reader(9762, b);
    for (generation, work, edits, quality) in [(1, 0, 8, "work-limit"), (2, 4096, 0, "edit-limit")] {
        let response = before.compare_retained(&after, owner(9762 + generation), generation,
            ReaderComparisonLimits { max_source_bytes: 4096, max_work: work, max_edit_distance: edits }, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let document = parse(response.as_str().as_bytes()).unwrap();
        assert!(!document.get("comparison_complete").flag());
        assert_eq!(document.get("comparison_quality").text(), quality);
        assert!(document.get("work_units").number() <= work);
        assert_ne!(document.get("relation").text(), "identical");
        assert!(document.get("spans").array().iter().any(|span| span.get("kind").text() == "unresolved"));
        ranges(&document, a, b);
    }
}

#[test]
fn refused_owner_limits_stale_and_canceled_attempts_preserve_source_and_query_state() {
    let mut before = reader(9771, b"old"); let after = reader(9772, b"new");
    for invalid in [9771, 9772] {
        assert_eq!(before.compare_retained(&after, owner(invalid), 1,
            ReaderComparisonLimits::default(), || false).err(), Some(ReaderSessionError::Comparison(ComparisonError::OwnerMismatch)));
    }
    assert_eq!(before.compare_retained(&after, owner(9773), 1,
        ReaderComparisonLimits { max_source_bytes: 2, ..ReaderComparisonLimits::default() }, || false).err(),
        Some(ReaderSessionError::Comparison(ComparisonError::Limits)));
    assert_eq!(before.compare_retained(&after, owner(9774), 1,
        ReaderComparisonLimits::default(), || true).err(), Some(ReaderSessionError::Canceled));
    assert_eq!(before.compare_retained(&after, owner(9775), 1,
        ReaderComparisonLimits::default(), || false).err(), Some(ReaderSessionError::Comparison(ComparisonError::Stale)));
    assert!(before.compare_retained(&after, owner(9776), 2, ReaderComparisonLimits::default(), || false).is_ok());
    assert_eq!(before.capture().bytes(), b"old"); assert_eq!(after.capture().bytes(), b"new");
}

#[test]
fn raw_native_path_receipts_never_replace_identity_with_a_display_label() {
    let a = Path::new(std::ffi::OsStr::from_bytes(b"same-\xff.rs"));
    let b = Path::new(std::ffi::OsStr::from_bytes(b"same-\xfe.rs"));
    let mut before = ReaderSession::from_bytes(owner(9781), a, b"same", || false).unwrap();
    let after = ReaderSession::from_bytes(owner(9782), b, b"same", || false).unwrap();
    let response = before.compare_retained(&after, owner(9783), 1, ReaderComparisonLimits::default(), || false).unwrap();
    let document = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(document.get("before_source").get("path").get("hex").text(), "73616d652dff2e7273");
    assert_eq!(document.get("after_source").get("path").get("hex").text(), "73616d652dfe2e7273");
    assert_eq!(document.get("relation").text(), "identical");
    assert_ne!(before.capture().request().file(), after.capture().request().file());
}

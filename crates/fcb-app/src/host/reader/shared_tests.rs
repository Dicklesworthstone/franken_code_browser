#![forbid(unsafe_code)]

use super::*;

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn source(value: u64, bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(value), Path::new("retained.rs"), bytes, || false).unwrap()
}

#[test]
fn forks_share_source_accounting_and_release_private_state_independently() {
    let bytes = vec![b'a'; MAX_HOST_TEXT_BYTES];
    let parent = source(9301, &bytes);
    let original_budget = parent.budget.clone();
    let source_reservation = parent._source_lease.info();
    let original_reserved = original_budget.accounting().reserved();
    let pointer = parent.capture.bytes().as_ptr();
    let child = parent.fork(owner(9302), || false).unwrap();
    let child_budget = child.budget.clone();
    let grandchild = child.fork(owner(9303), || false).unwrap();
    let grandchild_budget = grandchild.budget.clone();
    assert!(std::ptr::eq(pointer, child.capture.bytes().as_ptr()));
    assert!(std::ptr::eq(pointer, grandchild.capture.bytes().as_ptr()));
    assert_eq!(child._source_lease.info(), source_reservation);
    assert_eq!(grandchild._source_lease.info(), source_reservation);
    assert_eq!(original_budget.accounting().reserved(), original_reserved,
        "forking must not charge another source allocation");
    assert_eq!(child_budget.accounting().active_allocations(), 1);
    assert_eq!(grandchild_budget.accounting().active_allocations(), 1);
    assert_eq!(child_budget.accounting().reserved(), child._state_lease.info().bytes());
    assert_eq!(grandchild_budget.accounting().reserved(), grandchild._state_lease.info().bytes());
    assert!(child_budget.accounting().reserved().get() < bytes.len() as u64,
        "a pin's private state must not grow to the full source size");

    drop(parent);
    assert_eq!(original_budget.accounting().reserved(), source_reservation.bytes(),
        "the shared source remains charged after its originating reader closes");
    assert_eq!(original_budget.accounting().active_allocations(), 1);
    drop(child);
    assert_eq!(child_budget.accounting().reserved().get(), 0);
    assert_eq!(original_budget.accounting().reserved(), source_reservation.bytes());
    assert_eq!(grandchild.capture.bytes(), bytes.as_slice());
    let mut last = grandchild.fork(owner(9304), || false).unwrap();
    let last_budget = last.budget.clone();
    drop(grandchild);
    assert_eq!(grandchild_budget.accounting().reserved().get(), 0);
    assert!(std::ptr::eq(pointer, last.capture.bytes().as_ptr()));
    assert!(last.copy_range(0, 3, || false).unwrap().as_str().contains("616161"));
    drop(last);
    assert_eq!(last_budget.accounting().reserved().get(), 0);
    assert_eq!(original_budget.accounting().reserved().get(), 0,
        "the final source owner releases its retained reservation");
    assert_eq!(original_budget.accounting().active_allocations(), 0);
}

#[test]
fn pin_state_cost_is_independent_of_source_size_and_equal_supplies_stay_independent() {
    let small = source(9311, b"small");
    let large = source(9312, &vec![b'l'; MAX_HOST_TEXT_BYTES]);
    let small_pin = small.fork(owner(9313), || false).unwrap();
    let large_pin = large.fork(owner(9314), || false).unwrap();
    assert_eq!(small_pin.budget.accounting().reserved(), large_pin.budget.accounting().reserved());
    // Negative control: explicitly supplying equal content creates a different
    // source, not an implicit cross-owner cache hit authorized by a digest.
    let independent = source(9315, small.capture.bytes());
    assert_eq!(independent.capture.bytes(), small.capture.bytes());
    assert_eq!(independent.capture.digest(), small.capture.digest());
    assert!(!std::ptr::eq(independent.capture.bytes().as_ptr(), small.capture.bytes().as_ptr()));
    assert_ne!(independent._source_lease.info().domain(), small._source_lease.info().domain());
    assert!(std::ptr::eq(small_pin.capture.bytes().as_ptr(), small.capture.bytes().as_ptr()));
}

#[test]
fn shared_bytes_never_transfer_query_outline_or_document_authority() {
    let mut parent = source(9321, b"fn alpha() {}\nfn beta() {}\n");
    parent.search(7, "alpha", 8, 1024, || false).unwrap();
    parent.prepare_outline(11, ReaderOutlineOptions::default(), || false).unwrap();
    parent.prepare_document(17, ReaderDocumentOptions::default(), || false).unwrap();
    let mut child = parent.fork(owner(9322), || false).unwrap();
    assert_ne!(child.capture.request(), parent.capture.request());
    assert_eq!(child.owner(), owner(9322));
    assert_eq!(child.accepted_generation(), None);
    assert_eq!(child.outline_generation(), None);
    assert_eq!(child.document_generation(), None);
    assert_eq!(child.copy_hit(7, 0, || false).err(), Some(ReaderSessionError::MissingHit));
    assert_eq!(child.document_window(17, 0, 1, || false).err(), Some(ReaderSessionError::MissingDocument));
    child.search(1, "beta", 8, 1024, || false).unwrap();
    child.prepare_outline(1, ReaderOutlineOptions::default(), || false).unwrap();
    child.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
    assert_eq!(parent.accepted_generation(), Some(7));
    assert_eq!(parent.outline_generation(), Some(11));
    assert_eq!(parent.document_generation(), Some(17));
    assert_eq!(child.accepted_generation(), Some(1));
    assert!(parent.copy_hit(7, 0, || false).unwrap().as_str().contains("616c706861"));
    drop(parent);
    assert!(child.copy_hit(1, 0, || false).unwrap().as_str().contains("62657461"));
    assert!(child.document_window(1, 0, 1, || false).is_ok());
}

#[test]
fn cancellation_before_and_after_sharing_does_not_leak_source_reservations() {
    for stop_at in 1..=2 {
        let mut parent = source(9331, b"kept needle");
        parent.search(9, "needle", 8, 1024, || false).unwrap();
        let witness = parent.budget.clone();
        let before = witness.accounting().reserved();
        let mut polls = 0;
        let result = parent.fork(owner(9332), || {
            polls += 1;
            polls == stop_at
        });
        assert_eq!(result.err(), Some(ReaderSessionError::Canceled));
        assert_eq!(witness.accounting().reserved(), before);
        assert!(parent.copy_hit(9, 0, || false).is_ok());
        let child = parent.fork(owner(9333), || false).unwrap();
        drop(child);
        drop(parent);
        assert_eq!(witness.accounting().reserved().get(), 0,
            "a canceled fork must not keep the original source lease alive");
        assert_eq!(witness.accounting().active_allocations(), 0);
    }
}

#[test]
fn same_owner_refusal_preserves_the_existing_reader_and_allocation() {
    let parent = source(9341, b"retained");
    let witness = parent.budget.clone();
    let before = witness.accounting();
    assert_eq!(parent.fork(parent.owner(), || false).err(), Some(ReaderSessionError::InvalidLimits));
    assert_eq!(witness.accounting(), before);
    assert_eq!(parent.capture.bytes(), b"retained");
    drop(parent);
    assert_eq!(witness.accounting().reserved().get(), 0);
}

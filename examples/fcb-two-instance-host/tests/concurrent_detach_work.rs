#![forbid(unsafe_code)]

//! FCB-079.B: Execute independent close under concurrent work.
//!
//! Verifies view detachment and session close during concurrent asynchronous work:
//! 1. `test_01_detach_view_during_concurrent_query` — Detach view during background query.
//! 2. `test_02_detach_view_during_concurrent_upload` — Detach view during GPU terminal upload.
//! 3. `test_03_detach_view_during_concurrent_persistence` — Detach view during persistence transaction.
//! 4. `test_04_detach_view_during_simultaneous_query_upload_persistence` — Full concurrent triad.
//! 5. `test_05_reattach_view_and_cross_owner_reattach_oracle` — Reattach and cross-owner rejection.
//! 6. `test_06_independent_session_close_clears_concurrent_work` — Session close clears pending work cleanly.
//! 7. `test_07_negative_control_detached_operations_and_foreign_access` — Negative controls on detached views.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`].

use std::fs;
use std::path::PathBuf;

use fcb::ArenaOwnerId;
use fcb_core::RootId;
use fcb_runtime::terminal::{GpuSubmissionId, TerminalCompletionStatus};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use fcb_two_instance_host::{
    HostDeviceToken, HostFixtureError, TwoInstanceHost,
};

const RUN_ID_ENV: &str = "FCB_079_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-079-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_79_00_02),
        pin: SourcePin::new("0790007900079000790007900079000790007902").expect("pin valid"),
        route: RouteId::new("headless:embedding:concurrent-detach").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };

    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn create_test_fixture() -> (TwoInstanceHost, ArenaOwnerId, ArenaOwnerId) {
    let owner_a = ArenaOwnerId::new(0x0791).expect("owner a");
    let owner_b = ArenaOwnerId::new(0x0792).expect("owner b");
    let root_a = RootId::new(owner_a, 1).expect("root a");
    let root_b = RootId::new(owner_b, 1).expect("root b");

    let mut host = TwoInstanceHost::new(owner_a, owner_b, root_a, root_b)
        .expect("host fixture created");

    host.shared_provider().insert_file(
        "shared/math.rs",
        b"pub fn add(a: u32, b: u32) -> u32 { a + b }\n".to_vec(),
    );
    host.shared_provider().insert_file(
        "shared/types.rs",
        b"pub struct Point { pub x: f64, pub y: f64 }\n".to_vec(),
    );

    // Open initial views
    let _ = host.open_view_a("shared/math.rs").expect("open a");
    let _ = host.open_view_b("shared/types.rs").expect("open b");

    (host, owner_a, owner_b)
}

#[test]
fn test_01_detach_view_during_concurrent_query() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    assert!(host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // 1. Both instances initiate concurrent background queries
    host.start_query_a(101, "shared/math.rs").expect("start q a");
    host.start_query_b(201, "shared/types.rs").expect("start q b");
    assert_eq!(host.pending_queries_count_a(), 1);
    assert_eq!(host.pending_queries_count_b(), 1);

    // 2. Detach view A while query is in-flight
    let detached_a = host.detach_view_a().expect("detach a");
    assert_eq!(detached_a.source().owner(), owner_a);
    assert!(!host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // 3. Query A completes cleanly despite view detachment
    let capture_a = host.complete_query_a(101).expect("complete q a");
    assert_eq!(capture_a.owner(), owner_a);
    assert_eq!(
        capture_a.bytes(),
        b"pub fn add(a: u32, b: u32) -> u32 { a + b }\n"
    );

    // 4. View B and instance B remain attached and unaffected
    let capture_b = host.complete_query_b(201).expect("complete q b");
    assert_eq!(capture_b.owner(), owner_b);
    assert_eq!(
        capture_b.bytes(),
        b"pub struct Point { pub x: f64, pub y: f64 }\n"
    );

    // 5. Host run loop remains active
    assert!(host.run_loop().is_running());
    let (q_a, q_b) = host.shared_provider().audit_counters();
    assert_eq!(q_a, 2); // 1 initial open + 1 query
    assert_eq!(q_b, 2); // 1 initial open + 1 query

    record_receipt(
        "test_01_detach_view_during_concurrent_query",
        Effect::Succeeded,
        "detaching view A during in-flight query allows clean completion while view B and host remain valid",
    );
}

#[test]
fn test_02_detach_view_during_concurrent_upload() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    // 1. Reserve GPU terminal submissions in both queues
    let sub_a = GpuSubmissionId::next();
    let res_a = host.drain_queue_a_mut().reserve(sub_a).expect("reserve a");
    res_a.commit().expect("commit a");

    let sub_b = GpuSubmissionId::next();
    let res_b = host.drain_queue_b_mut().reserve(sub_b).expect("reserve b");
    res_b.commit().expect("commit b");

    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 1);
    assert_eq!(host.drain_queue_b_mut().in_flight_count(), 1);

    // 2. Detach view A while GPU upload is in flight
    let _detached_a = host.detach_view_a().expect("detach a");
    assert!(!host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // In-flight upload reservation survives detachment until terminal ownership permits release
    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 1);

    // 3. Complete and drain upload A
    host.drain_queue_a_mut()
        .record_completion(sub_a, TerminalCompletionStatus::Cancelled)
        .expect("record cancel a");
    let drain_a = host.drain_queue_a_mut().drain_completed();
    assert_eq!(drain_a.cancelled_drained, 1);
    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 0);

    // 4. View B upload completes successfully
    host.drain_queue_b_mut()
        .record_completion(sub_b, TerminalCompletionStatus::Success)
        .expect("record success b");
    let drain_b = host.drain_queue_b_mut().drain_completed();
    assert_eq!(drain_b.completed_drained, 1);
    assert_eq!(host.drain_queue_b_mut().in_flight_count(), 0);

    // 5. Host device tokens remain valid
    assert!(host.device_a().validate_for(owner_a).is_ok());
    assert!(host.device_b().validate_for(owner_b).is_ok());
    assert!(host.run_loop().is_running());

    record_receipt(
        "test_02_detach_view_during_concurrent_upload",
        Effect::Succeeded,
        "GPU submission leases survive view detachment until terminal ownership drain discharges them",
    );
}

#[test]
fn test_03_detach_view_during_concurrent_persistence() {
    let (mut host, _, _) = create_test_fixture();

    // 1. Both instances begin in-flight persistence transactions
    host.begin_persistence_tx_a(1, "bookmark:main", "offset:100")
        .expect("begin tx a");
    host.begin_persistence_tx_b(2, "bookmark:main", "offset:999")
        .expect("begin tx b");
    assert_eq!(host.pending_persistence_count_a(), 1);
    assert_eq!(host.pending_persistence_count_b(), 1);

    // 2. Detach view A while persistence transaction is in flight
    let _ = host.detach_view_a().expect("detach a");
    assert!(!host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // 3. Commit transactions for both instances
    host.commit_persistence_tx_a(1).expect("commit tx a");
    host.commit_persistence_tx_b(2).expect("commit tx b");

    assert_eq!(host.pending_persistence_count_a(), 0);
    assert_eq!(host.pending_persistence_count_b(), 0);

    // 4. Privacy barrier: annotations remain completely private
    assert_eq!(host.annotation_a("bookmark:main"), Some("offset:100"));
    assert_eq!(host.annotation_b("bookmark:main"), Some("offset:999"));

    record_receipt(
        "test_03_detach_view_during_concurrent_persistence",
        Effect::Succeeded,
        "persistence transactions commit cleanly across detached view boundary with strict privacy separation",
    );
}

#[test]
fn test_04_detach_view_during_simultaneous_query_upload_persistence() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    // 1. Setup simultaneous triad on both instances:
    // (a) Background queries
    host.start_query_a(11, "shared/math.rs").expect("start q a");
    host.start_query_b(22, "shared/types.rs").expect("start q b");

    // (b) GPU upload submissions
    let sub_a = GpuSubmissionId::next();
    let res_a = host.drain_queue_a_mut().reserve(sub_a).expect("reserve sub a");
    res_a.commit().expect("commit sub a");

    let sub_b = GpuSubmissionId::next();
    let res_b = host.drain_queue_b_mut().reserve(sub_b).expect("reserve sub b");
    res_b.commit().expect("commit sub b");

    // (c) Persistence transactions
    host.begin_persistence_tx_a(501, "setting:theme", "dark").expect("tx a");
    host.begin_persistence_tx_b(502, "setting:theme", "light").expect("tx b");

    // 2. Detach view A while all three operations are in flight
    let detached_a = host.detach_view_a().expect("detach a");
    assert_eq!(detached_a.source().owner(), owner_a);
    assert!(!host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // 3. Drain and settle instance A operations
    let capture_a = host.complete_query_a(11).expect("complete q a");
    assert_eq!(capture_a.owner(), owner_a);

    host.drain_queue_a_mut()
        .record_completion(sub_a, TerminalCompletionStatus::Cancelled)
        .expect("cancel sub a");
    let drain_a = host.drain_queue_a_mut().drain_completed();
    assert_eq!(drain_a.cancelled_drained, 1);

    host.commit_persistence_tx_a(501).expect("commit tx a");
    assert_eq!(host.annotation_a("setting:theme"), Some("dark"));

    // Redraw for detached view A fails with ViewNotAttached
    let redraw_a = host.request_redraw_view_a();
    assert_eq!(
        redraw_a,
        Err(HostFixtureError::ViewNotAttached { owner: owner_a })
    );

    // 4. Settle instance B operations (all succeed normally)
    let capture_b = host.complete_query_b(22).expect("complete q b");
    assert_eq!(capture_b.owner(), owner_b);

    host.drain_queue_b_mut()
        .record_completion(sub_b, TerminalCompletionStatus::Success)
        .expect("success sub b");
    let drain_b = host.drain_queue_b_mut().drain_completed();
    assert_eq!(drain_b.completed_drained, 1);

    host.commit_persistence_tx_b(502).expect("commit tx b");
    assert_eq!(host.annotation_b("setting:theme"), Some("light"));

    // Redraw for attached view B succeeds
    assert!(host.request_redraw_view_b().is_ok());

    record_receipt(
        "test_04_detach_view_during_simultaneous_query_upload_persistence",
        Effect::Succeeded,
        "triad of simultaneous in-flight query, upload, and persistence operations drains cleanly on detach",
    );
}

#[test]
fn test_05_reattach_view_and_cross_owner_reattach_oracle() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    let view_a = host.detach_view_a().expect("detach a");
    assert!(!host.is_view_a_attached());

    // 1. Cross-owner reattach attempt: reattaching view A into instance B fails with OwnerMismatch
    let cross_reattach = host.reattach_view_b(view_a.clone());
    assert_eq!(
        cross_reattach,
        Err(HostFixtureError::OwnerMismatch {
            expected: owner_b,
            actual: owner_a,
        })
    );

    // 2. Legitimate reattach of view A into instance A succeeds
    assert!(host.reattach_view_a(view_a).is_ok());
    assert!(host.is_view_a_attached());

    // 3. Redraw request for reattached view A now succeeds
    assert!(host.request_redraw_view_a().is_ok());

    // 4. Detach view B and test reverse cross-owner rejection
    let view_b = host.detach_view_b().expect("detach b");
    let cross_reattach_rev = host.reattach_view_a(view_b.clone());
    assert_eq!(
        cross_reattach_rev,
        Err(HostFixtureError::OwnerMismatch {
            expected: owner_a,
            actual: owner_b,
        })
    );
    assert!(host.reattach_view_b(view_b).is_ok());

    record_receipt(
        "test_05_reattach_view_and_cross_owner_reattach_oracle",
        Effect::Succeeded,
        "oracle rejects cross-owner view reattachment and restores redraw capabilities upon valid reattach",
    );
}

#[test]
fn test_06_independent_session_close_clears_concurrent_work() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    // 1. Queue pending queries, uploads, and persistence txs on A
    host.start_query_a(1, "shared/math.rs").expect("q a");
    let sub_a = GpuSubmissionId::next();
    let res_a = host.drain_queue_a_mut().reserve(sub_a).expect("sub a");
    res_a.commit().expect("commit sub a");
    host.begin_persistence_tx_a(10, "key", "val").expect("tx a");

    // Queue pending work on B as well
    host.start_query_b(2, "shared/types.rs").expect("q b");
    host.begin_persistence_tx_b(20, "key_b", "val_b").expect("tx b");

    // 2. Close instance A independently while work is in flight
    let summary_a = host.close_instance_a().expect("close a");
    assert_eq!(summary_a.owner, owner_a);
    assert_eq!(summary_a.cancelled_queries, 1);
    assert_eq!(summary_a.discarded_persistence_txs, 1);
    assert!(summary_a.host_still_running);
    assert!(summary_a.peer_still_active);

    // Instance A is inactive
    assert!(!host.is_instance_a_active());
    assert!(!host.is_view_a_attached());

    // Instance B remains active and operational
    assert!(host.is_instance_b_active());
    assert!(host.is_view_b_attached());
    assert_eq!(host.pending_queries_count_b(), 1);
    assert_eq!(host.pending_persistence_count_b(), 1);

    // Complete B's pending query and commit its tx
    let cap_b = host.complete_query_b(2).expect("complete b");
    assert_eq!(cap_b.owner(), owner_b);
    host.commit_persistence_tx_b(20).expect("commit b");
    assert_eq!(host.annotation_b("key_b"), Some("val_b"));

    record_receipt(
        "test_06_independent_session_close_clears_concurrent_work",
        Effect::Succeeded,
        "closing instance A safely drains and discards uncommitted work while peer B continues smoothly",
    );
}

#[test]
fn test_07_negative_control_detached_operations_and_foreign_access() {
    let (mut host, owner_a, _) = create_test_fixture();

    // 1. Detach view A once succeeds
    assert!(host.detach_view_a().is_ok());

    // 2. Double detach returns ViewAlreadyDetached
    let double_detach = host.detach_view_a();
    assert_eq!(
        double_detach,
        Err(HostFixtureError::ViewAlreadyDetached { owner: owner_a })
    );

    // 3. Committing non-existent persistence tx returns PendingTransactionNotFound
    let non_existent_tx = host.commit_persistence_tx_a(9999);
    assert_eq!(
        non_existent_tx,
        Err(HostFixtureError::PendingTransactionNotFound { tx_id: 9999 })
    );

    // 4. Completing non-existent query returns PendingQueryNotFound
    let non_existent_query = host.complete_query_a(9999);
    assert_eq!(
        non_existent_query,
        Err(HostFixtureError::PendingQueryNotFound { query_id: 9999 })
    );

    // 5. Foreign owner token rejection
    let foreign = ArenaOwnerId::new(0x9999).expect("foreign");
    let foreign_device = HostDeviceToken::new(888, foreign);
    assert_eq!(
        foreign_device.validate_for(owner_a),
        Err(HostFixtureError::OwnerMismatch {
            expected: owner_a,
            actual: foreign,
        })
    );

    record_receipt(
        "test_07_negative_control_detached_operations_and_foreign_access",
        Effect::Succeeded,
        "negative control demonstrates oracle catches double detachment, missing transactions, and foreign device tokens",
    );
}

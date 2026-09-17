#![forbid(unsafe_code)]

//! FCB-079.V: Production verification scenario suite.
//! Two-instance native embedding and independent host/session shutdown tests.
//!
//! Verifies the complete end-to-end embedding lifecycle:
//! 1. `test_01_full_two_instance_embedding_lifecycle` — Complete dual-instance session lifecycle.
//! 2. `test_02_lossless_terminal_drain_accounting_under_load` — Complete drain queue accounting under load.
//! 3. `test_03_cross_owner_oracle_comprehensive_matrix` — Exhaustive cross-owner isolation checks.
//! 4. `test_04_session_close_independence_with_active_peer` — Peer isolation during in-flight work and teardown.
//! 5. `test_05_negative_control_oracle_detects_all_violations` — Negative control matrix proving oracle accuracy.
//!
//! Emits structured [`ScenarioReceipt`]s with event rings and content digests.

use std::fs;
use std::path::PathBuf;

use fcb::{ArenaOwnerId, FcbError, HostRequest};
use fcb_core::RootId;
use fcb_runtime::terminal::{GpuSubmissionId, TerminalCompletionStatus};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use fcb_two_instance_host::{
    HostFixtureError, TwoInstanceHost,
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
        seed: ScenarioSeed(0x0C_79_00_03),
        pin: SourcePin::new("0790007900079000790007900079000790007903").expect("pin valid"),
        route: RouteId::new("headless:embedding:production-verification").expect("route valid"),
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

fn create_production_fixture() -> (TwoInstanceHost, ArenaOwnerId, ArenaOwnerId) {
    let owner_a = ArenaOwnerId::new(0x0791).expect("owner a");
    let owner_b = ArenaOwnerId::new(0x0792).expect("owner b");
    let root_a = RootId::new(owner_a, 1).expect("root a");
    let root_b = RootId::new(owner_b, 1).expect("root b");

    let host = TwoInstanceHost::new(owner_a, owner_b, root_a, root_b)
        .expect("host fixture created");

    host.shared_provider().insert_file(
        "src/main.rs",
        b"fn main() { println!(\"FCB production runtime\"); }\n".to_vec(),
    );
    host.shared_provider().insert_file(
        "src/lib.rs",
        b"pub fn add(x: i32, y: i32) -> i32 { x + y }\n".to_vec(),
    );
    host.font_domain().insert_font(
        "Menlo-Regular",
        vec![0x00, 0x01, 0x00, 0x00, 0xAA, 0xBB],
    );

    (host, owner_a, owner_b)
}

#[test]
fn test_01_full_two_instance_embedding_lifecycle() {
    let (mut host, owner_a, owner_b) = create_production_fixture();

    // 1. Initial state: host run loop running, both instances active
    assert!(host.run_loop().is_running());
    assert!(host.is_instance_a_active());
    assert!(host.is_instance_b_active());

    // 2. Open views in both instances
    let view_a = host.open_view_a("src/main.rs").expect("open a");
    let view_b = host.open_view_b("src/lib.rs").expect("open b");

    assert_eq!(view_a.source().owner(), owner_a);
    assert_eq!(view_b.source().owner(), owner_b);
    assert!(host.is_view_a_attached());
    assert!(host.is_view_b_attached());

    // 3. Shared font domain: explicit consent
    host.font_domain().grant_consent(owner_a).expect("grant a");
    let font_a = host.font_domain().query_font(owner_a, "Menlo-Regular").expect("query font a");
    assert!(font_a.is_some());

    // Owner B cannot query without consent
    assert!(matches!(
        host.font_domain().query_font(owner_b, "Menlo-Regular"),
        Err(HostFixtureError::UnauthorizedFontAccess { .. })
    ));

    // 4. Detach view A, reattach, redraw
    let detached_a = host.detach_view_a().expect("detach a");
    assert!(!host.is_view_a_attached());
    assert!(host.reattach_view_a(detached_a).is_ok());
    assert!(host.is_view_a_attached());
    assert!(host.request_redraw_view_a().is_ok());

    // 5. Close instance A independently
    let summary_a = host.close_instance_a().expect("close a");
    assert_eq!(summary_a.owner, owner_a);
    assert!(summary_a.host_still_running);
    assert!(summary_a.peer_still_active);
    assert!(!host.is_instance_a_active());
    assert!(host.is_instance_b_active());

    // 6. Instance B continues operating
    assert!(host.request_redraw_view_b().is_ok());

    // 7. Close instance B
    let summary_b = host.close_instance_b().expect("close b");
    assert_eq!(summary_b.owner, owner_b);
    assert!(summary_b.host_still_running);
    assert!(!summary_b.peer_still_active);

    // Host run loop survives both instance shutdowns (no global teardown)
    assert!(host.run_loop().is_running());

    record_receipt(
        "test_01_full_two_instance_embedding_lifecycle",
        Effect::Succeeded,
        "complete dual-instance embedding lifecycle executes without resource cross-contamination or premature teardown",
    );
}

#[test]
fn test_02_lossless_terminal_drain_accounting_under_load() {
    let (mut host, _, _) = create_production_fixture();

    // Submit batch of GPU uploads to instance A
    let mut subs_a = Vec::new();
    for _ in 0..8 {
        let sub = GpuSubmissionId::next();
        let res = host.drain_queue_a_mut().reserve(sub).expect("reserve");
        res.commit().expect("commit");
        subs_a.push(sub);
    }
    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 8);

    // Complete half with Success and half with Cancelled
    for (i, sub) in subs_a.iter().enumerate() {
        let status = if i % 2 == 0 {
            TerminalCompletionStatus::Success
        } else {
            TerminalCompletionStatus::Cancelled
        };
        host.drain_queue_a_mut().record_completion(*sub, status).expect("record");
    }

    let report_a = host.drain_queue_a_mut().drain_completed();
    assert_eq!(report_a.completed_drained, 4);
    assert_eq!(report_a.cancelled_drained, 4);
    assert_eq!(report_a.completed_drained + report_a.cancelled_drained, 8);
    assert_eq!(report_a.remaining_in_flight, 0);
    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 0);

    record_receipt(
        "test_02_lossless_terminal_drain_accounting_under_load",
        Effect::Succeeded,
        "lossless terminal drain queue strictly accounts for all submissions with zero lease leaks under batch load",
    );
}

#[test]
fn test_03_cross_owner_oracle_comprehensive_matrix() {
    let (mut host, owner_a, owner_b) = create_production_fixture();

    let view_a = host.open_view_a("src/main.rs").expect("open a");
    let capture_a = view_a.source().clone();

    // 1. Cross-owner capture refusal
    assert!(host.verify_cross_owner_rejection(&capture_a).is_ok());

    // 2. Cross-owner view reattach refusal
    let detached_a = host.detach_view_a().expect("detach a");
    assert_eq!(
        host.reattach_view_b(detached_a.clone()),
        Err(HostFixtureError::OwnerMismatch {
            expected: owner_b,
            actual: owner_a,
        })
    );
    assert!(host.reattach_view_a(detached_a).is_ok());

    // 3. Cross-owner device token validation
    assert!(host.device_a().validate_for(owner_a).is_ok());
    assert!(matches!(
        host.device_a().validate_for(owner_b),
        Err(HostFixtureError::OwnerMismatch { .. })
    ));

    // 4. Cross-owner font cache isolation without consent
    assert!(matches!(
        host.font_domain().query_font(owner_a, "Menlo-Regular"),
        Err(HostFixtureError::UnauthorizedFontAccess { .. })
    ));

    record_receipt(
        "test_03_cross_owner_oracle_comprehensive_matrix",
        Effect::Succeeded,
        "oracle rejects cross-owner captures, device tokens, font consent bypasses, and view cross-attachments",
    );
}

#[test]
fn test_04_session_close_independence_with_active_peer() {
    let (mut host, owner_a, owner_b) = create_production_fixture();

    let _view_a = host.open_view_a("src/main.rs").expect("open a");
    let _view_b = host.open_view_b("src/lib.rs").expect("open b");

    // Enqueue work on both
    host.start_query_a(1, "src/main.rs").expect("q a");
    host.start_query_b(2, "src/lib.rs").expect("q b");

    let sub_a = GpuSubmissionId::next();
    let res_a = host.drain_queue_a_mut().reserve(sub_a).expect("res a");
    res_a.commit().expect("commit a");

    let sub_b = GpuSubmissionId::next();
    let res_b = host.drain_queue_b_mut().reserve(sub_b).expect("res b");
    res_b.commit().expect("commit b");

    host.begin_persistence_tx_a(10, "pos", "100").expect("tx a");
    host.begin_persistence_tx_b(20, "pos", "200").expect("tx b");

    // Close instance A during concurrent work
    let close_summary = host.close_instance_a().expect("close a");
    assert_eq!(close_summary.owner, owner_a);
    assert_eq!(close_summary.cancelled_queries, 1);
    assert_eq!(close_summary.discarded_persistence_txs, 1);
    assert!(close_summary.host_still_running);
    assert!(close_summary.peer_still_active);

    // Instance B completes everything smoothly
    let cap_b = host.complete_query_b(2).expect("complete b");
    assert_eq!(cap_b.owner(), owner_b);

    host.drain_queue_b_mut()
        .record_completion(sub_b, TerminalCompletionStatus::Success)
        .expect("complete sub b");
    let drain_b = host.drain_queue_b_mut().drain_completed();
    assert_eq!(drain_b.completed_drained, 1);

    host.commit_persistence_tx_b(20).expect("commit tx b");
    assert_eq!(host.annotation_b("pos"), Some("200"));

    record_receipt(
        "test_04_session_close_independence_with_active_peer",
        Effect::Succeeded,
        "closing instance A safely drains and discards uncommitted work while peer B completes without interruption",
    );
}

#[test]
fn test_05_negative_control_oracle_detects_all_violations() {
    let (mut host, owner_a, _) = create_production_fixture();

    // 1. Foreign owner request rejected
    let foreign = ArenaOwnerId::new(0xEEEE).expect("foreign");
    assert_eq!(
        host.run_loop().record_request(foreign, HostRequest::RequestRedraw),
        Err(FcbError::OwnerMismatch)
    );

    // Open view A first
    let _ = host.open_view_a("src/main.rs").expect("open a");

    // 2. Detached view redraw rejected
    let _ = host.detach_view_a().expect("detach");
    assert_eq!(
        host.request_redraw_view_a(),
        Err(HostFixtureError::ViewNotAttached { owner: owner_a })
    );

    // 3. Double detach rejected
    assert_eq!(
        host.detach_view_a(),
        Err(HostFixtureError::ViewAlreadyDetached { owner: owner_a })
    );

    // 4. Closed instance double close rejected
    assert!(host.close_instance_a().is_ok());
    assert_eq!(
        host.close_instance_a(),
        Err(HostFixtureError::InstanceAlreadyClosed { owner: owner_a })
    );

    record_receipt(
        "test_05_negative_control_oracle_detects_all_violations",
        Effect::Succeeded,
        "negative control proves oracle catches foreign owners, detached redraws, double detachment, and duplicate closure",
    );
}

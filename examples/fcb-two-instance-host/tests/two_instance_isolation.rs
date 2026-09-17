#![forbid(unsafe_code)]

//! FCB-079.A unit and integration test suite:
//! Two-instance native embedding and independent host/session shutdown tests.
//!
//! Required cases:
//! 1. `test_01_separate_grants_ids_and_devices` — Separate grants, IDs, and device tokens.
//! 2. `test_02_shared_source_provider_independent_namespaces` — Shared provider access with per-instance accounting.
//! 3. `test_03_authorized_font_domain_with_per_instance_consent` — Shared font cache with consent and privacy barrier.
//! 4. `test_04_cross_owner_handles_rejected_by_oracle` — Rejection of cross-owner captures, handles, and device tokens.
//! 5. `test_05_independent_close_during_inflight_work` — Close instance A while in-flight work drains, leaving B and host alive.
//! 6. `test_06_no_global_resource_teardown_after_both_close` — Both instances close without tearing down host run loop.
//! 7. `test_07_negative_control_double_close_and_foreign_owner` — Rejection of duplicate closes and foreign owner requests.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`].

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
        seed: ScenarioSeed(0x0C_79_00_01),
        pin: SourcePin::new("0790007900079000790007900079000790007900").expect("pin valid"),
        route: RouteId::new("headless:embedding:two-instance").expect("route valid"),
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

    let host = TwoInstanceHost::new(owner_a, owner_b, root_a, root_b)
        .expect("host fixture created");

    host.shared_provider().insert_file(
        "shared/math.rs",
        b"pub fn add(a: u32, b: u32) -> u32 { a + b }\n".to_vec(),
    );
    host.shared_provider().insert_file(
        "shared/types.rs",
        b"pub struct Point { pub x: f64, pub y: f64 }\n".to_vec(),
    );

    (host, owner_a, owner_b)
}

#[test]
fn test_01_separate_grants_ids_and_devices() {
    let (host, owner_a, owner_b) = create_test_fixture();

    assert_eq!(host.owner_a(), owner_a);
    assert_eq!(host.owner_b(), owner_b);
    assert_ne!(host.owner_a(), host.owner_b());

    assert_eq!(host.root_a().owner(), owner_a);
    assert_eq!(host.root_b().owner(), owner_b);
    assert_ne!(host.root_a(), host.root_b());

    let dev_a = host.device_a();
    let dev_b = host.device_b();
    assert_eq!(dev_a.owner(), owner_a);
    assert_eq!(dev_b.owner(), owner_b);
    assert_ne!(dev_a.id(), dev_b.id());

    assert!(dev_a.validate_for(owner_a).is_ok());
    assert!(dev_b.validate_for(owner_b).is_ok());
    assert!(matches!(
        dev_a.validate_for(owner_b),
        Err(HostFixtureError::OwnerMismatch { .. })
    ));

    record_receipt(
        "test_01_separate_grants_ids_and_devices",
        Effect::Succeeded,
        "two-instance host allocates separate owner namespaces, root grants, and typed device tokens",
    );
}

#[test]
fn test_02_shared_source_provider_independent_namespaces() {
    let (mut host, _, _) = create_test_fixture();

    let view_a = host.open_view_a("shared/math.rs").expect("open a");
    assert_eq!(
        view_a.source().bytes(),
        b"pub fn add(a: u32, b: u32) -> u32 { a + b }\n"
    );

    let view_b = host.open_view_b("shared/types.rs").expect("open b");
    assert_eq!(
        view_b.source().bytes(),
        b"pub struct Point { pub x: f64, pub y: f64 }\n"
    );

    let (q_a, q_b) = host.shared_provider().audit_counters();
    assert_eq!(q_a, 1);
    assert_eq!(q_b, 1);

    record_receipt(
        "test_02_shared_source_provider_independent_namespaces",
        Effect::Succeeded,
        "shared provider serves independent views with per-instance audit counters",
    );
}

#[test]
fn test_03_authorized_font_domain_with_per_instance_consent() {
    let (host, owner_a, owner_b) = create_test_fixture();
    let fonts = host.font_domain();

    fonts.insert_font("SFMono-Regular", vec![0x00, 0x01, 0x00, 0x00]);

    // 1. Without consent, font access is refused
    let unauth_a = fonts.query_font(owner_a, "SFMono-Regular");
    assert!(matches!(
        unauth_a,
        Err(HostFixtureError::UnauthorizedFontAccess { .. })
    ));

    // 2. Grant consent to A only
    fonts.grant_consent(owner_a).expect("grant a");
    let font_a = fonts.query_font(owner_a, "SFMono-Regular").expect("query a");
    assert!(font_a.is_some());

    // B still refused
    let unauth_b = fonts.query_font(owner_b, "SFMono-Regular");
    assert!(matches!(
        unauth_b,
        Err(HostFixtureError::UnauthorizedFontAccess { .. })
    ));

    // 3. Grant consent to B
    fonts.grant_consent(owner_b).expect("grant b");
    let font_b = fonts.query_font(owner_b, "SFMono-Regular").expect("query b");
    assert!(font_b.is_some());

    // 4. Revoke consent for A
    fonts.revoke_consent(owner_a);
    let revoked_a = fonts.query_font(owner_a, "SFMono-Regular");
    assert!(matches!(
        revoked_a,
        Err(HostFixtureError::UnauthorizedFontAccess { .. })
    ));

    let (c_a, c_b) = fonts.audit_counters();
    assert_eq!(c_a, 1);
    assert_eq!(c_b, 1);

    record_receipt(
        "test_03_authorized_font_domain_with_per_instance_consent",
        Effect::Succeeded,
        "shared font domain enforces per-instance consent and tracks privacy audit counters",
    );
}

#[test]
fn test_04_cross_owner_handles_rejected_by_oracle() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    let view_a = host.open_view_a("shared/math.rs").expect("open a");
    let capture_a = view_a.source().clone();

    // Verify cross-owner oracle
    let oracle_res = host.verify_cross_owner_rejection(&capture_a);
    assert!(oracle_res.is_ok());

    // Test private annotation isolation
    host.set_annotation_a("bookmark_1", "line:10");
    host.set_annotation_b("bookmark_1", "line:42");

    assert_eq!(host.annotation_a("bookmark_1"), Some("line:10"));
    assert_eq!(host.annotation_b("bookmark_1"), Some("line:42"));

    // Foreign device token check
    let rogue_device = HostDeviceToken::new(999, ArenaOwnerId::new(0xDEAD).expect("rogue"));
    assert!(matches!(
        rogue_device.validate_for(owner_a),
        Err(HostFixtureError::OwnerMismatch { .. })
    ));
    assert!(matches!(
        rogue_device.validate_for(owner_b),
        Err(HostFixtureError::OwnerMismatch { .. })
    ));

    record_receipt(
        "test_04_cross_owner_handles_rejected_by_oracle",
        Effect::Succeeded,
        "cross-owner handles, captures, device tokens, and annotations are strictly isolated and rejected",
    );
}

#[test]
fn test_05_independent_close_during_inflight_work() {
    let (mut host, owner_a, owner_b) = create_test_fixture();

    // Open views in both instances
    let _view_a = host.open_view_a("shared/math.rs").expect("open a");
    let _view_b = host.open_view_b("shared/types.rs").expect("open b");

    // Enqueue in-flight GPU submission in instance A
    let sub_a = GpuSubmissionId::next();
    let res_a = host.drain_queue_a_mut().reserve(sub_a).expect("reserve a");
    let slot_a = res_a.slot_id();
    assert!(slot_a.get() > 0);
    res_a.commit().expect("commit a");
    assert_eq!(host.drain_queue_a_mut().in_flight_count(), 1);

    // Enqueue in-flight GPU submission in instance B
    let sub_b = GpuSubmissionId::next();
    let res_b = host.drain_queue_b_mut().reserve(sub_b).expect("reserve b");
    let slot_b = res_b.slot_id();
    assert!(slot_b.get() > 0);
    res_b.commit().expect("commit b");
    assert_eq!(host.drain_queue_b_mut().in_flight_count(), 1);

    // Cancel submission A and mark completion before close
    host.drain_queue_a_mut()
        .record_completion(sub_a, TerminalCompletionStatus::Cancelled)
        .expect("record cancel a");

    // Close instance A while work is in flight
    let close_report_a = host.close_instance_a().expect("close a");
    assert_eq!(close_report_a.owner, owner_a);
    assert_eq!(close_report_a.drain_report.cancelled_drained, 1);
    assert!(close_report_a.host_still_running);
    assert!(close_report_a.peer_still_active);

    assert!(!host.is_instance_a_active());
    assert!(host.is_instance_b_active());
    assert!(host.run_loop().is_running());

    // Instance B remains fully functional and its in-flight work completes successfully
    host.drain_queue_b_mut()
        .record_completion(sub_b, TerminalCompletionStatus::Success)
        .expect("b completes");
    let drain_b = host.drain_queue_b_mut().drain_completed();
    assert_eq!(drain_b.completed_drained, 1);
    assert_eq!(host.drain_queue_b_mut().in_flight_count(), 0);

    // Host run loop continues servicing instance B requests
    host.run_loop()
        .record_request(owner_b, HostRequest::RequestRedraw)
        .expect("b redraw");
    let (_, _, _, redraws_b) = host.run_loop().audit_counters();
    assert_eq!(redraws_b, 1);

    record_receipt(
        "test_05_independent_close_during_inflight_work",
        Effect::Succeeded,
        "closing instance A during in-flight work drains cleanly while instance B and host loop remain valid",
    );
}

#[test]
fn test_06_no_global_resource_teardown_after_both_close() {
    let (mut host, _, owner_b) = create_test_fixture();

    let _ = host.close_instance_a().expect("close a");
    let close_report_b = host.close_instance_b().expect("close b");

    assert_eq!(close_report_b.owner, owner_b);
    assert!(close_report_b.host_still_running);
    assert!(!close_report_b.peer_still_active);

    assert!(!host.is_instance_a_active());
    assert!(!host.is_instance_b_active());

    // Host run loop survives both session closures without global teardown
    assert!(host.run_loop().is_running());
    let now = host.run_loop().tick(50_000);
    assert!(now >= 1_000_000);

    record_receipt(
        "test_06_no_global_resource_teardown_after_both_close",
        Effect::Succeeded,
        "closing both instances preserves host run loop and does not perform global resource destruction",
    );
}

#[test]
fn test_07_negative_control_double_close_and_foreign_owner() {
    let (mut host, owner_a, _) = create_test_fixture();

    // 1. First close succeeds
    assert!(host.close_instance_a().is_ok());

    // 2. Second close fails cleanly with InstanceAlreadyClosed
    let double_close = host.close_instance_a();
    assert!(matches!(
        double_close,
        Err(HostFixtureError::InstanceAlreadyClosed { owner }) if owner == owner_a
    ));

    // 3. Foreign owner request to host run loop fails with OwnerMismatch
    let foreign = ArenaOwnerId::new(0xCAFE).expect("foreign");
    let foreign_req = host.run_loop().record_request(foreign, HostRequest::Wake);
    assert_eq!(foreign_req, Err(FcbError::OwnerMismatch));

    record_receipt(
        "test_07_negative_control_double_close_and_foreign_owner",
        Effect::Succeeded,
        "oracle catches duplicate session closure and rejects foreign owner requests",
    );
}

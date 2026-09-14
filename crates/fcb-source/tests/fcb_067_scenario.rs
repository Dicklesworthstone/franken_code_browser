//! FCB-067.V joint conformance campaign: exercises the host provider and
//! capture capability types (FCB-067.A) and the in-memory provider
//! conformance boundary (FCB-067.B) together, through the izu8 scenario
//! driver. Runs headless; native routes report missing evidence rather
//! than fabricated passes.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::{
    CancelFlag, CaptureGuarantees, CaptureConsistency, CaptureOutcome, CaptureRequest,
    CompleteCapture, HostSourceProvider, InMemorySourceProvider, RangeReadSupport,
    ReadOrdering, SourceError, SourceGrant,
};
use fcb_test_support::receipts::Redactor;
use fcb_test_support::scenario::{
    BodyResult, RequiredOutcome, ScenarioContext, ScenarioDriver, ScenarioRoute, ScenarioSpec,
    ScenarioVerdict,
};

fn owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
}

fn capture_request(owner_id: u64, file_value: u64, revision: u64) -> CaptureRequest {
    let owner_id = owner(owner_id);
    CaptureRequest::new(
        FileId::new(owner_id, file_value).unwrap(),
        SourceRevision::new(owner_id, revision).unwrap(),
    )
    .unwrap()
}

fn provider_with(owner_id: u64, payload: &[u8]) -> InMemorySourceProvider {
    let mut provider =
        InMemorySourceProvider::new(owner(owner_id), ByteLength::new(u64::MAX));
    let bytes: Arc<[u8]> = Arc::from(payload.to_vec().into_boxed_slice());
    provider
        .insert(
            FileId::new(owner(owner_id), 5).unwrap(),
            SourceRevision::new(owner(owner_id), 1).unwrap(),
            bytes,
        )
        .unwrap();
    provider
}

/// Joint scenario 1: a full capture delivered by the conformance provider
/// is admitted by the capability vocabulary and carries the exact bytes.
fn joint_full_capture_round_trip(context: &ScenarioContext) -> BodyResult {
    let grant = SourceGrant::new(owner(81))
        .grant(FileId::new(owner(81), 5).unwrap())
        .unwrap();
    let provider = provider_with(81, b"exact captured bytes");
    let request = capture_request(81, 5, 1)
        .with_range(ByteRange::new(ByteOffset::new(0), ByteOffset::new(20)).unwrap())
        .unwrap();
    let outcome = provider
        .capture(&grant, &request, &CancelFlag::new())
        .expect("full capture admitted");
    grant.validate(&outcome).expect("grant admits own capture");
    match outcome {
        CaptureOutcome::Complete(complete) => {
            let _ = context.write_fixture("full-capture.bytes", complete.bytes());
            assert_eq!(complete.bytes(), b"exact captured bytes");
            BodyResult::Pass
        }
        CaptureOutcome::Extent(_) => BodyResult::Fail {
            reason: "whole-range request must produce a complete capture".to_string(),
        },
    }
}

/// Joint scenario 2: a range request returns exactly the requested slice
/// with truthful declared length, bounded by the provider payload.
fn joint_range_read_is_exact_slice(context: &ScenarioContext) -> BodyResult {
    let grant = SourceGrant::new(owner(82))
        .grant(FileId::new(owner(82), 5).unwrap())
        .unwrap();
    let provider = provider_with(82, b"0123456789");
    let request = capture_request(82, 5, 1)
        .with_range(ByteRange::new(ByteOffset::new(2), ByteOffset::new(7)).unwrap())
        .unwrap();
    let outcome = match provider.capture(&grant, &request, &CancelFlag::new()) {
        Ok(outcome) => outcome,
        Err(error) => {
            return BodyResult::Fail {
                reason: format!("range capture refused: {error}"),
            }
        }
    };
    grant.validate(&outcome).expect("own capture admitted");
    match outcome {
        CaptureOutcome::Complete(complete) => {
            assert_eq!(complete.bytes(), b"23456");
            assert_eq!(complete.declared_length().get(), 5);
            let _ = context.write_fixture("range-slice.bytes", complete.bytes());
            BodyResult::Pass
        }
        CaptureOutcome::Extent(_) => BodyResult::Fail {
            reason: "in-memory provider delivers complete slices".to_string(),
        },
    }
}

/// Joint scenario 3: hostile metadata from the provider seam is refused at
/// acceptance instead of being admitted.
fn joint_hostile_metadata_is_refused(_context: &ScenarioContext) -> BodyResult {
    let bytes = Arc::from(b"12345".to_vec().into_boxed_slice());
    let declared = ByteLength::new(500);
    match CompleteCapture::new(capture_request(83, 1, 1), declared, bytes) {
        Err(SourceError::MetadataMismatch) => BodyResult::Pass,
        Err(other) => BodyResult::Fail {
            reason: format!("expected metadata mismatch, got {other}"),
        },
        Ok(_) => BodyResult::Fail {
            reason: "hostile metadata was admitted".to_string(),
        },
    }
}

/// Joint scenario 4: an ungranted foreign capture is rejected by the grant
/// boundary — two independently granted sources cannot alias.
fn joint_foreign_capture_is_rejected(_context: &ScenarioContext) -> BodyResult {
    let grant = SourceGrant::new(owner(85));
    let foreign = CompleteCapture::new(
        capture_request(86, 9, 1),
        ByteLength::new(1),
        Arc::from(b"z".to_vec().into_boxed_slice()),
    )
    .unwrap();
    match grant.validate(&CaptureOutcome::Complete(foreign)) {
        Err(SourceError::ForeignOwner) => BodyResult::Pass,
        Err(other) => BodyResult::Fail {
            reason: format!("expected foreign-owner refusal, got {other}"),
        },
        Ok(()) => BodyResult::Fail {
            reason: "foreign capture was admitted".to_string(),
        },
    }
}

/// Negative control: an intentionally failing scenario proves the campaign
/// detects failures rather than passing everything.
fn joint_negative_control(_context: &ScenarioContext) -> BodyResult {
    BodyResult::ExpectedFailure {
        reason: "intentional negative control".to_string(),
    }
}

fn spec(
    name: &'static str,
    negative_control: bool,
    body: fn(&ScenarioContext) -> BodyResult,
) -> ScenarioSpec {
    ScenarioSpec {
        name,
        route: ScenarioRoute::Headless,
        timeout: std::time::Duration::from_secs(30),
        negative_control,
        seed: 67,
        body,
    }
}

#[test]
fn fcb_067_joint_campaign_passes_with_detected_negative_control() {
    let mut driver = ScenarioDriver::new(8, false).unwrap();
    driver
        .submit(spec(
            "joint_full_capture_round_trip",
            false,
            joint_full_capture_round_trip,
        ))
        .unwrap();
    driver
        .submit(spec(
            "joint_range_read_is_exact_slice",
            false,
            joint_range_read_is_exact_slice,
        ))
        .unwrap();
    driver
        .submit(spec(
            "joint_hostile_metadata_is_refused",
            false,
            joint_hostile_metadata_is_refused,
        ))
        .unwrap();
    driver
        .submit(spec(
            "joint_foreign_capture_is_rejected",
            false,
            joint_foreign_capture_is_rejected,
        ))
        .unwrap();
    driver
        .submit(spec(
            "joint_negative_control",
            true,
            joint_negative_control,
        ))
        .unwrap();

    let redactor = Redactor::new();
    let records = driver.run_pending(&redactor);

    let requirements = [
        (
            "joint_full_capture_round_trip",
            RequiredOutcome::Pass,
        ),
        (
            "joint_range_read_is_exact_slice",
            RequiredOutcome::Pass,
        ),
        (
            "joint_hostile_metadata_is_refused",
            RequiredOutcome::Pass,
        ),
        (
            "joint_foreign_capture_is_rejected",
            RequiredOutcome::Pass,
        ),
        (
            "joint_negative_control",
            RequiredOutcome::ExpectedFailure,
        ),
    ];
    for (name, required) in &requirements {
        let record = records
            .iter()
            .find(|record| record.name == *name)
            .unwrap_or_else(|| panic!("scenario {name} produced no record"));
        let satisfied = match required {
            RequiredOutcome::Pass => record.verdict == ScenarioVerdict::Passed,
            RequiredOutcome::ExpectedFailure => {
                record.verdict == ScenarioVerdict::ExpectedFailure
            }
        };
        assert!(
            satisfied,
            "scenario {name} produced {:?}; replay {:?}",
            record.verdict,
            record.replay_command()
        );
        // Every campaign receipt must survive the codec round trip.
        let decoded =
            fcb_test_support::receipts::ScenarioReceipt::decode(&record.receipt.encode())
                .expect("receipt decodes");
        assert_eq!(decoded, record.receipt);
    }

    // The guarantees statement of the provider seam is part of the joint
    // contract: declared defaults infer nothing.
    let provider = provider_with(81, b"guarantees");
    assert_eq!(
        HostSourceProvider::guarantees(&provider),
        CaptureGuarantees {
            consistency: CaptureConsistency::ObservedSequence,
            ordering: ReadOrdering::StablePerRevision,
            range_reads: RangeReadSupport::ByteRanges,
            cancellation: fcb_source::CancellationSupport::Cooperative,
        }
    );

    driver.finish().unwrap();
}

//! Focused seam tests for FCB-067.A: the [`HostSourceProvider`] trait path
//! with hostile metadata and two independently granted sources, exercised
//! headlessly with no native initialization.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::{
    gate_request_on_cancel, CancelFlag, CaptureGuarantees, CaptureOutcome, CaptureRequest,
    CompleteCapture, HostSourceProvider, ObservedRange, SourceError, SourceGrant,
};

fn owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
}

fn file(owner_id: ArenaOwnerId, value: u64) -> FileId {
    FileId::new(owner_id, value).unwrap()
}

fn request_for(owner_id: u64, file_value: u64, start: u64, end: u64) -> CaptureRequest {
    let owner_id = owner(owner_id);
    CaptureRequest::new(file(owner_id, file_value), SourceRevision::new(owner_id, 1).unwrap())
        .unwrap()
        .with_range(ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap())
        .unwrap()
}

/// An honest provider: delivered bytes always match declared metadata.
struct HonestProvider {
    payload: &'static [u8],
}

impl HostSourceProvider for HonestProvider {
    fn guarantees(&self) -> CaptureGuarantees {
        CaptureGuarantees::minimal()
    }

    fn capture(
        &self,
        _grant: &SourceGrant,
        request: &CaptureRequest,
        cancel: &CancelFlag,
    ) -> Result<CaptureOutcome, SourceError> {
        gate_request_on_cancel(cancel)?;
        let range = request.range().expect("focused tests always request a range");
        if range.end().get() as usize > self.payload.len() {
            return Err(SourceError::RangeOutOfBounds);
        }
        let bytes: Arc<[u8]> = Arc::from(self.payload.to_vec().into_boxed_slice());
        let complete = CompleteCapture::new(*request, ByteLength::new(bytes.len() as u64), bytes)?;
        Ok(CaptureOutcome::Complete(complete))
    }
}

/// A hostile provider: declares one length and delivers another.
struct LyingProvider {
    delivered: usize,
}

impl HostSourceProvider for LyingProvider {
    fn guarantees(&self) -> CaptureGuarantees {
        CaptureGuarantees::minimal()
    }

    fn capture(
        &self,
        _grant: &SourceGrant,
        request: &CaptureRequest,
        cancel: &CancelFlag,
    ) -> Result<CaptureOutcome, SourceError> {
        gate_request_on_cancel(cancel)?;
        let bytes: Arc<[u8]> = Arc::from(vec![b'x'; self.delivered].into_boxed_slice());
        let complete = CompleteCapture::new(*request, ByteLength::new(999), bytes)?;
        Ok(CaptureOutcome::Complete(complete))
    }
}

#[test]
fn honest_provider_capture_is_admitted_and_owned_by_grant() {
    let grant = SourceGrant::new(owner(71))
        .grant(file(owner(71), 5))
        .unwrap();
    let provider = HonestProvider {
        payload: b"exact bytes",
    };
    let request = request_for(71, 5, 0, 11);
    let outcome = provider.capture(&grant, &request, &CancelFlag::new()).unwrap();
    grant.validate(&outcome).unwrap();
    match outcome {
        CaptureOutcome::Complete(complete) => {
            assert_eq!(complete.bytes(), b"exact bytes");
            assert_eq!(complete.declared_length().get(), 11);
        }
        CaptureOutcome::Extent(_) => panic!("honest provider delivers complete captures"),
    }
}

#[test]
fn lying_provider_metadata_is_refused_at_acceptance() {
    let grant = SourceGrant::new(owner(72))
        .grant(file(owner(72), 6))
        .unwrap();
    let provider = LyingProvider { delivered: 3 };
    let request = request_for(72, 6, 0, 3);
    // The provider itself must refuse its own inconsistent delivery.
    let outcome = provider.capture(&grant, &request, &CancelFlag::new());
    assert_eq!(outcome, Err(SourceError::MetadataMismatch));
}

#[test]
fn foreign_provider_delivery_is_rejected_by_grant() {
    let grant = SourceGrant::new(owner(73))
        .grant(file(owner(73), 7))
        .unwrap();
    // A hostile host hands back a capture naming a foreign owner domain.
    let foreign_request = request_for(74, 7, 0, 1);
    let bytes: Arc<[u8]> = Arc::from(b"y".to_vec().into_boxed_slice());
    let foreign = CompleteCapture::new(foreign_request, ByteLength::new(1), bytes).unwrap();
    assert_eq!(
        grant.validate(&CaptureOutcome::Complete(foreign)),
        Err(SourceError::ForeignOwner)
    );
}

#[test]
fn two_independently_granted_sources_stay_separate_through_the_seam() {
    let grant_a = SourceGrant::new(owner(75))
        .grant(file(owner(75), 8))
        .unwrap();
    let grant_b = SourceGrant::new(owner(76))
        .grant(file(owner(76), 8))
        .unwrap();
    let provider = HonestProvider {
        payload: b"same value, different owners",
    };
    let request_a = request_for(75, 8, 0, 4);
    let request_b = request_for(76, 8, 0, 4);
    let outcome_a = provider.capture(&grant_a, &request_a, &CancelFlag::new()).unwrap();
    let outcome_b = provider.capture(&grant_b, &request_b, &CancelFlag::new()).unwrap();
    grant_a.validate(&outcome_a).unwrap();
    grant_b.validate(&outcome_b).unwrap();
    // Cross-validation must fail both ways: grants are independent scopes.
    assert_eq!(
        grant_a.validate(&outcome_b),
        Err(SourceError::ForeignOwner)
    );
    assert_eq!(
        grant_b.validate(&outcome_a),
        Err(SourceError::ForeignOwner)
    );
}

#[test]
fn cancellation_before_capture_is_honored_without_native_initialization() {
    let grant = SourceGrant::new(owner(77))
        .grant(file(owner(77), 9))
        .unwrap();
    let provider = HonestProvider {
        payload: b"never delivered",
    };
    let request = request_for(77, 9, 0, 4);
    let cancel = CancelFlag::new();
    cancel.cancel();
    assert_eq!(
        provider.capture(&grant, &request, &cancel),
        Err(SourceError::Canceled)
    );
}

#[test]
fn extent_delivery_from_provider_carries_observations_and_holes() {
    let grant = SourceGrant::new(owner(78))
        .grant(file(owner(78), 10))
        .unwrap();
    let request = request_for(78, 10, 0, 8);
    // A provider that could only observe part of the file delivers an
    // extent capture: observed [0,3) and [5,8), hole [3,5), total 8.
    let observations = vec![
        ObservedRange::new(
            ByteRange::new(ByteOffset::new(0), ByteOffset::new(3)).unwrap(),
            b"abc",
        )
        .unwrap(),
        ObservedRange::new(
            ByteRange::new(ByteOffset::new(5), ByteOffset::new(8)).unwrap(),
            b"fgh",
        )
        .unwrap(),
    ];
    let holes = vec![ByteRange::new(ByteOffset::new(3), ByteOffset::new(5)).unwrap()];
    let extent = fcb_source::ExtentCapture::new(
        request,
        observations,
        holes,
        Some(ByteLength::new(8)),
    )
    .unwrap();
    let outcome = CaptureOutcome::Extent(extent);
    grant.validate(&outcome).unwrap();
    if let CaptureOutcome::Extent(extent) = outcome {
        assert!(extent.covers(ByteRange::new(ByteOffset::new(0), ByteOffset::new(3)).unwrap()));
        assert!(!extent.covers(ByteRange::new(ByteOffset::new(2), ByteOffset::new(6)).unwrap()));
        assert_eq!(extent.holes().len(), 1);
    }
}

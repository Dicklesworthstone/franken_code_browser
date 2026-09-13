use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fcb::{
    ArenaOwnerId, BrowserSession, FcbError, Feature, FileId, MemorySourceProvider,
    SourceCapture, SourceProvider, SourceRevision,
};

#[test]
fn independent_consumer_uses_real_in_memory_source_without_implicit_probe() {
    struct Probe {
        calls: Arc<AtomicUsize>,
        owner: ArenaOwnerId,
    }

    impl SourceProvider for Probe {
        fn capture(&self, logical_path: &str) -> Result<SourceCapture, FcbError> {
            assert_eq!(logical_path, "memory.rs");
            self.calls.fetch_add(1, Ordering::SeqCst);
            SourceCapture::from_bytes(
                self.owner,
                FileId::new(self.owner, 7).expect("non-zero file identity"),
                SourceRevision::new(self.owner, 11).expect("non-zero source revision"),
                logical_path,
                b"fn main() {}".to_vec(),
            )
        }
    }

    let owner = ArenaOwnerId::new(101).expect("non-zero owner");
    let calls = Arc::new(AtomicUsize::new(0));
    let session = BrowserSession::with_provider(
        owner,
        Arc::new(Probe {
            calls: Arc::clone(&calls),
            owner,
        }),
    );

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let view = session.open("memory.rs").expect("explicit open succeeds");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(view.source().bytes(), b"fn main() {}");
    assert_eq!(view.frame_plan().expect("frame plan").bytes().len().get(), 12);
    drop(view);
    drop(session);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn public_consumer_exposes_boundary_refusals() {
    let owner = ArenaOwnerId::new(202).expect("non-zero owner");
    let other_owner = ArenaOwnerId::new(303).expect("non-zero owner");
    let mut provider = MemorySourceProvider::new(owner).expect("provider");

    assert_eq!(provider.insert("", Vec::new()), Err(FcbError::InvalidPath));
    assert_eq!(provider.insert("nul\0.rs", Vec::new()), Err(FcbError::InvalidPath));
    provider
        .insert("same.rs", b"one".to_vec())
        .expect("first source");
    assert_eq!(
        provider.insert("same.rs", b"two".to_vec()),
        Err(FcbError::DuplicateSource)
    );
    assert_eq!(provider.capture("missing.rs"), Err(FcbError::SourceNotFound));

    let session = BrowserSession::with_provider(owner, Arc::new(provider));
    assert_eq!(session.open("missing.rs"), Err(FcbError::SourceNotFound));
    assert_eq!(
        BrowserSession::new(owner).open("any.rs"),
        Err(FcbError::ProviderUnavailable)
    );

    let foreign_capture = SourceCapture::from_bytes(
        other_owner,
        FileId::new(other_owner, 1).expect("non-zero file identity"),
        SourceRevision::new(other_owner, 1).expect("non-zero source revision"),
        "foreign.rs",
        b"foreign".to_vec(),
    )
    .expect("foreign capture");
    assert_eq!(
        BrowserSession::new(owner).open_capture(foreign_capture),
        Err(FcbError::OwnerMismatch)
    );
}

#[test]
fn all_feature_selection_keeps_unimplemented_capabilities_unavailable() {
    let owner = ArenaOwnerId::new(404).expect("non-zero owner");
    let session = BrowserSession::new(owner);
    let available = session.available_features();

    for feature in Feature::ALL {
        let target_supported = feature != Feature::MacosMetal || cfg!(target_os = "macos");
        let expected = if !target_supported {
            Err(FcbError::UnsupportedTarget)
        } else if feature.implemented() {
            Ok(())
        } else {
            Err(FcbError::FeatureUnavailable)
        };
        assert_eq!(
            session.require_feature(feature),
            expected,
            "feature {} must agree with its implementation status",
            feature.name()
        );
        assert_eq!(
            available.contains(feature),
            feature.implemented() && target_supported,
            "feature {} availability must not be inferred from Cargo selection",
            feature.name()
        );
    }
}

#[test]
fn frame_identity_distinguishes_same_owner_equal_revision_files() {
    let owner = ArenaOwnerId::new(505).expect("non-zero owner");
    let revision = SourceRevision::new(owner, 1).expect("non-zero source revision");
    let capture_a = SourceCapture::from_bytes(
        owner,
        FileId::new(owner, 1).expect("non-zero file identity"),
        revision,
        "a.rs",
        b"same".to_vec(),
    )
    .expect("first capture");
    let capture_b = SourceCapture::from_bytes(
        owner,
        FileId::new(owner, 2).expect("non-zero file identity"),
        revision,
        "b.rs",
        b"same".to_vec(),
    )
    .expect("second capture");

    let view_a = BrowserSession::new(owner)
        .open_capture(capture_a)
        .expect("first view");
    let view_b = BrowserSession::new(owner)
        .open_capture(capture_b)
        .expect("second view");
    assert_ne!(
        view_a.source().file(),
        view_b.source().file(),
        "source identities must retain distinct file ids"
    );
    assert_ne!(
        view_a.frame_plan().expect("first frame plan"),
        view_b.frame_plan().expect("second frame plan"),
        "frame identity must include FileId, not only owner/revision/range"
    );
}

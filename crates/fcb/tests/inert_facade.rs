use std::sync::Arc;

use fcb::{
    ArenaOwnerId, BrowserSession, FcbError, Feature, FeatureSet, FileId, MemorySourceProvider,
    SourceCapture, SourceProvider, SourceRevision,
};

#[test]
fn independent_consumers_keep_sources_and_ids_separate() {
    let owner_a = ArenaOwnerId::new(101).unwrap();
    let owner_b = ArenaOwnerId::new(202).unwrap();
    let mut provider_a = MemorySourceProvider::new(owner_a).unwrap();
    let mut provider_b = MemorySourceProvider::new(owner_b).unwrap();
    provider_a.insert("a.rs", b"alpha".to_vec()).unwrap();
    provider_b.insert("b.rs", b"beta".to_vec()).unwrap();

    let session_a = BrowserSession::with_provider(owner_a, Arc::new(provider_a));
    let session_b = BrowserSession::with_provider(owner_b, Arc::new(provider_b));
    let view_a = session_a.open("a.rs").unwrap();
    let view_b = session_b.open("b.rs").unwrap();
    assert_ne!(view_a.source().file(), view_b.source().file());
    assert_ne!(view_a.source().revision(), view_b.source().revision());
    assert_eq!(view_a.frame_plan().unwrap().bytes().len().get(), 5);
    assert_eq!(view_b.frame_plan().unwrap().bytes().len().get(), 4);
}

#[test]
fn positive_host_capture_and_explicit_close_are_side_effect_free() {
    let owner = ArenaOwnerId::new(303).unwrap();
    let capture = SourceCapture::from_bytes(
        owner,
        FileId::new(owner, 9).unwrap(),
        SourceRevision::new(owner, 11).unwrap(),
        "empty.rs",
        Vec::new(),
    )
    .unwrap();
    let session = BrowserSession::new(owner);
    let view = session.open_capture(capture).unwrap();
    assert!(view.source().bytes().is_empty());
    let closed = BrowserSession::new(owner).close();
    assert_eq!(closed.owner(), owner);
}

#[test]
fn boundary_and_failure_cases_are_observable() {
    let owner = ArenaOwnerId::new(404).unwrap();
    let mut provider = MemorySourceProvider::new(owner).unwrap();
    assert_eq!(provider.insert("", Vec::new()), Err(FcbError::InvalidPath));
    assert_eq!(provider.insert("nul\0.rs", Vec::new()), Err(FcbError::InvalidPath));
    provider.insert("same.rs", b"one".to_vec()).unwrap();
    assert_eq!(provider.insert("same.rs", b"two".to_vec()), Err(FcbError::DuplicateSource));
    assert_eq!(provider.capture("missing.rs"), Err(FcbError::SourceNotFound));

    let session = BrowserSession::with_provider(owner, Arc::new(provider));
    assert_eq!(session.open("missing.rs"), Err(FcbError::SourceNotFound));
    assert_eq!(BrowserSession::new(owner).open("any.rs"), Err(FcbError::ProviderUnavailable));
    let other_owner = ArenaOwnerId::new(405).unwrap();
    let mut foreign_provider = MemorySourceProvider::new(other_owner).unwrap();
    foreign_provider.insert("foreign.rs", b"foreign".to_vec()).unwrap();
    let foreign_capture = foreign_provider.capture("foreign.rs").unwrap();
    assert_eq!(BrowserSession::new(owner).open_capture(foreign_capture), Err(FcbError::OwnerMismatch));
}

#[test]
fn feature_union_reports_only_compile_selected_capabilities() {
    let compiled = FeatureSet::compiled();
    for feature in Feature::ALL {
        assert_eq!(compiled.contains(feature), feature.compiled());
    }
}

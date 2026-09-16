use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fcb::{
    ArenaOwnerId, BrowserSession, FcbError, Feature, FeatureSet, HostRequest, HostServices,
    MemorySourceProvider,
};

struct ServiceProbe {
    calls: Arc<AtomicUsize>,
    capabilities: FeatureSet,
}

impl HostServices for ServiceProbe {
    fn capabilities(&self) -> FeatureSet {
        self.capabilities
    }

    fn monotonic_nanos(&self) -> u64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        42
    }

    fn request(&self, _: HostRequest) -> Result<(), FcbError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn external_consumer_selects_services_without_implicit_work() {
    let owner = ArenaOwnerId::new(701).unwrap();
    let mut provider = MemorySourceProvider::new(owner).unwrap();
    provider.insert("memory.rs", b"host-selected".to_vec()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let services: Arc<dyn HostServices> = Arc::new(ServiceProbe {
        calls: Arc::clone(&calls),
        capabilities: FeatureSet::all_known(),
    });

    let session = BrowserSession::with_provider_and_services(
        owner,
        Arc::new(provider),
        services,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(session.host_capabilities(), FeatureSet::available());
    let view = session.open("memory.rs").unwrap();
    assert_eq!(view.source().bytes(), b"host-selected");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    assert_eq!(session.host_monotonic_nanos(), Ok(42));
    assert_eq!(session.request_host(HostRequest::RequestRedraw), Ok(()));
    assert_eq!(session.request_host(HostRequest::Wake), Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    drop(view);
    drop(session);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn absent_host_services_refuse_clock_and_wake_requests() {
    let session = BrowserSession::new(ArenaOwnerId::new(702).unwrap());
    assert!(session.host_capabilities().is_empty());
    assert_eq!(
        session.host_monotonic_nanos(),
        Err(FcbError::HostServicesUnavailable)
    );
    assert_eq!(
        session.request_host(HostRequest::RequestRedraw),
        Err(FcbError::HostServicesUnavailable)
    );
}

#[test]
fn host_capability_union_cannot_advertise_unimplemented_profiles() {
    let owner = ArenaOwnerId::new(703).unwrap();
    let services = Arc::new(ServiceProbe {
        calls: Arc::new(AtomicUsize::new(0)),
        capabilities: FeatureSet::all_known(),
    });
    let session = BrowserSession::with_services(owner, services);
    let available = session.host_capabilities();

    for feature in Feature::ALL {
        assert_eq!(available.contains(feature), feature.implemented() && feature.target_supported());
    }
    assert_eq!(session.require_feature(Feature::Search), if cfg!(feature = "search") { Ok(()) } else { Err(FcbError::FeatureUnavailable) });
    assert_eq!(session.require_feature(Feature::Map), if cfg!(feature = "map") { Ok(()) } else { Err(FcbError::FeatureUnavailable) });
    assert_eq!(session.require_feature(Feature::Markdown), Err(FcbError::FeatureUnavailable));
    assert_eq!(session.require_feature(Feature::Runtime), Err(FcbError::FeatureUnavailable));
    assert_eq!(session.require_feature(Feature::Persistence), Err(FcbError::FeatureUnavailable));
    assert_eq!(session.require_feature(Feature::Source), Ok(()));
    assert_eq!(session.require_feature(Feature::View), Ok(()));
    assert_eq!(
        session.require_feature(Feature::MacosMetal),
        if cfg!(target_os = "macos") {
            Err(FcbError::FeatureUnavailable)
        } else {
            Err(FcbError::UnsupportedTarget)
        }
    );
}

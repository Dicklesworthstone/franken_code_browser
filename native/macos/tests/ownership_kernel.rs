#[cfg(not(target_os = "macos"))]
use franken_macos::MetalLayer;
use franken_macos::{BridgeError, MainThreadToken, MetalDevice};

#[test]
fn public_entry_points_do_not_fabricate_native_objects_off_macos() {
    // On a real host, cargo test workers are not the AppKit main thread, so
    // a legitimate capture fails; the main-thread-bound contracts are then
    // exercised by the native qualification lane (FCB-003.v). Off-macOS the
    // synthetic policy admits capture so the refusal contract is testable.
    let token = match MainThreadToken::capture_current() {
        Ok(token) => token,
        Err(BridgeError::NotMainThread) => return,
        Err(other) => panic!("unexpected capture failure: {other:?}"),
    };

    #[cfg(not(target_os = "macos"))]
    {
        assert!(matches!(
            MetalLayer::new(token),
            Err(BridgeError::UnsupportedPlatform)
        ));
        assert!(matches!(
            MetalDevice::system_default(token),
            Err(BridgeError::UnsupportedPlatform)
        ));
    }
    #[cfg(target_os = "macos")]
    {
        // On the host platform the entry points are real; construction and
        // ownership pairing are asserted by the native qualification lane.
        let _ = token;
    }
}

#[test]
fn public_token_is_copyable_but_native_objects_remain_result_bound() {
    // Copy semantics of the token itself are platform-independent, but the
    // only public constructor requires the host main thread; on a cargo
    // worker thread of a real host, capture correctly fails.
    let token = match MainThreadToken::capture_current() {
        Ok(token) => token,
        Err(BridgeError::NotMainThread) => return,
        Err(other) => panic!("unexpected capture failure: {other:?}"),
    };
    let copied = token;
    assert_eq!(token, copied);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires the host AppKit main thread and a real Metal device; runs in the native qualification lane (FCB-003.v)"]
fn system_default_device_adopts_the_sdk_retained_result_once() {
    let token = MainThreadToken::capture_current().expect("test thread is the host thread");
    let device = MetalDevice::system_default(token).expect("Metal device is available");
    let ownership = device.ownership();
    assert_eq!(ownership.live_wrappers, 1);
    assert_eq!(ownership.retains, 0);
    assert_eq!(ownership.releases, 0);
}

#[test]
fn exception_policy_is_explicitly_unqualified() {
    assert!(franken_macos::PANIC_EXCEPTION_POLICY.contains("unavailable"));
    assert!(franken_macos::PANIC_EXCEPTION_POLICY.contains("unqualified"));
}

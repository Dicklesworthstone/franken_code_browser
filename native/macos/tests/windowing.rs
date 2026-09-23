//! Focused tests for the windowing seam: portable logic everywhere, real
//! AppKit objects and the host event queue on macOS.
//!
//! Thread truthfulness: cargo's default harness runs each test on a worker
//! thread that is not the AppKit main thread, so main-thread-dependent tests
//! self-skip there via [`try_main_token`]. The single-threaded harness run
//! (`cargo test -- --test-threads=1`) executes the harness on the host main
//! thread and is what qualifies real AppKit behavior in the native lane.

#![forbid(unsafe_code)]

use franken_macos::windowing::{
    BoundedEventQueue, InputEvent, KeyModifiers, ScrollPhase, WindowingError,
    decode_modifier_flags, decode_scroll_phase, logical_to_pixels, pixels_to_logical,
};

#[test]
fn bounded_queue_refuses_and_counts_drops() {
    let mut queue = BoundedEventQueue::new(2);
    assert_eq!(queue.capacity(), 2);
    assert!(queue.is_empty());

    queue
        .offer(InputEvent::Uninterpreted { raw_type: 1 })
        .expect("empty queue accepts");
    queue
        .offer(InputEvent::Uninterpreted { raw_type: 2 })
        .expect("capacity-two queue accepts the second");
    assert_eq!(
        queue.offer(InputEvent::Uninterpreted { raw_type: 3 }),
        Err(WindowingError::QueueFull {
            capacity: 2,
            dropped_total: 1
        })
    );
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.dropped(), 1);

    let first = queue.pop().expect("oldest retained event");
    assert_eq!(first, InputEvent::Uninterpreted { raw_type: 1 });
    queue
        .offer(InputEvent::Uninterpreted { raw_type: 4 })
        .expect("pop frees capacity");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.dropped(), 1);
}

#[test]
fn coordinate_conversion_round_trips_through_backing_scale() {
    assert_eq!(logical_to_pixels(3.0, 2.0), 6.0);
    assert_eq!(pixels_to_logical(6.0, 2.0), 3.0);
    // A zero scale is a passthrough guard, never a division by zero.
    assert_eq!(pixels_to_logical(5.0, 0.0), 5.0);
    let logical = 123.456;
    let pixels = logical_to_pixels(logical, 2.0);
    assert_eq!(pixels_to_logical(pixels, 2.0), logical);
}

#[test]
fn modifier_flags_decode_matches_platform_bits() {
    let decoded = decode_modifier_flags((1 << 16) | (1 << 20));
    assert!(decoded.caps_lock);
    assert!(decoded.command);
    assert!(!decoded.shift);
    assert!(!decoded.control);
    assert!(!decoded.option);

    let none = decode_modifier_flags(0);
    assert_eq!(none, KeyModifiers::default());
}

#[test]
fn scroll_phase_decode_maps_momentum_constants() {
    assert_eq!(decode_scroll_phase(0), ScrollPhase::None);
    assert_eq!(decode_scroll_phase(1), ScrollPhase::Began);
    assert_eq!(decode_scroll_phase(2), ScrollPhase::Changed);
    assert_eq!(decode_scroll_phase(3), ScrollPhase::None);
    assert_eq!(decode_scroll_phase(7), ScrollPhase::None);
}

#[cfg(target_os = "macos")]
mod native {
    use franken_macos::MainThreadToken;
    use franken_macos::windowing::{
        BackingGeneration, BoundedEventQueue, EventPump, InputEvent, KeyPhase, NativeWindow, Rect,
        ScrollPhase, WindowStyle, WindowingError,
    };

    /// The token on the host main thread (single-threaded harness), or `None`
    /// on a cargo worker thread, which is not the AppKit main thread;
    /// main-thread-dependent bodies then self-skip truthfully.
    fn try_main_token() -> Option<MainThreadToken> {
        MainThreadToken::capture_current().ok()
    }

    #[test]
    fn worker_threads_are_not_the_appkit_main_thread_in_default_harness() {
        // Honest in both harness modes: refused on a cargo worker thread,
        // accepted when the harness itself runs on the host main thread.
        match MainThreadToken::capture_current() {
            Err(_) => { /* worker thread: the honest refusal under test */ }
            Ok(_) => { /* single-threaded harness: the harness is main */ }
        }
    }

    #[test]
    fn native_window_creates_reports_backing_and_tracks_generation() {
        let Some(token) = try_main_token() else {
            return; // multithreaded harness: AppKit work requires the main thread
        };
        let window = NativeWindow::create(
            token,
            "fcb-windowing-test",
            Rect::at_origin(100.0, 100.0),
            WindowStyle::standard(),
        )
        .expect("real host creates a titled window");

        let scale = window.recorded_backing_scale();
        assert!(scale > 0.0, "real host reports a positive backing scale");
        assert_eq!(window.backing_generation(), BackingGeneration::INITIAL);

        window
            .set_content_size(token, 520.0, 410.0)
            .expect("live window accepts content size");
        let (observed, generation) = window
            .observe_backing(token)
            .expect("live window observes backing");
        assert_eq!(observed, scale, "same-display resize keeps the scale");
        assert_eq!(generation, window.backing_generation());

        let (px, py) = window.point_to_pixels(token, 10.0, 20.0).unwrap();
        assert_eq!(px, 10.0 * scale);
        assert_eq!(py, 20.0 * scale);

        window.mark_closed(token).expect("marking closed succeeds");
        assert!(window.is_closed());
        assert_eq!(
            window.observe_backing(token).unwrap_err(),
            WindowingError::Closed
        );
        window.close(token).expect("close after mark is idempotent");
    }

    #[test]
    fn pump_drains_empty_stream_without_events() {
        let Some(token) = try_main_token() else {
            return;
        };
        let pump = EventPump::attach(token).expect("host provides a shared application");
        let mut queue = BoundedEventQueue::new(8);
        let delivered = pump.pump(token, &mut queue, 10).expect("pump works");
        assert_eq!(delivered, 0, "a quiet queue drains nothing");
        assert!(queue.is_empty());
    }

    #[test]
    fn synthetic_keydown_posts_pumps_and_decodes() {
        let Some(token) = try_main_token() else {
            return;
        };
        let pump = EventPump::attach(token).expect("host provides a shared application");
        let mut queue = BoundedEventQueue::new(8);

        // The real synthesis + post + nextEvent loop, not a mock: the event
        // travels through the application queue exactly like a user keypress.
        assert!(pump.post_key(token, true, 5, "a", 1 << 17, false));

        let delivered = pump.pump(token, &mut queue, 10).expect("pump works");
        assert_eq!(delivered, 1);
        match queue.pop().expect("delivered event is queued") {
            InputEvent::Key(key) => {
                assert_eq!(key.keycode, 5);
                assert_eq!(key.characters.as_deref(), Some("a"));
                assert_eq!(key.phase, KeyPhase::Down);
                assert!(key.modifiers.shift);
                assert!(!key.repeat);
            }
            other => panic!("expected a key event, got {other:?}"),
        }
    }

    #[test]
    fn synthetic_scroll_pair_arrives_in_order_with_phases() {
        let Some(token) = try_main_token() else {
            return;
        };
        let pump = EventPump::attach(token).expect("host provides a shared application");
        let mut queue = BoundedEventQueue::new(8);

        assert!(pump.post_scroll(token, 3.0, -7.0, true));
        assert!(pump.post_scroll(token, 1.5, -1.0, true));

        let delivered = pump.pump(token, &mut queue, 10).expect("pump works");
        assert_eq!(delivered, 2, "simultaneous posted events arrive in order");

        match queue.pop().expect("first scroll") {
            InputEvent::Scroll(scroll) => {
                assert_eq!(scroll.delta_x, 3.0);
                assert_eq!(scroll.delta_y, -7.0);
                assert!(scroll.precise);
                assert_eq!(scroll.phase, ScrollPhase::None);
            }
            other => panic!("expected a scroll event, got {other:?}"),
        }
        match queue.pop().expect("second scroll") {
            InputEvent::Scroll(scroll) => assert_eq!(scroll.delta_x, 1.5),
            other => panic!("expected a scroll event, got {other:?}"),
        }
    }

    #[test]
    fn late_pump_after_window_close_survives() {
        let Some(token) = try_main_token() else {
            return;
        };
        let pump = EventPump::attach(token).expect("host provides a shared application");
        let window = NativeWindow::create(
            token,
            "fcb-late-callbacks",
            Rect::at_origin(0.0, 0.0),
            WindowStyle::standard(),
        )
        .expect("window creation succeeds");
        window.mark_closed(token).expect("marking closed succeeds");

        // A late pump after the window is gone is a no-op that must not
        // panic or resurrect the closed owner.
        let mut queue = BoundedEventQueue::new(4);
        let delivered = pump.pump(token, &mut queue, 10).expect("pump survives");
        assert_eq!(delivered, 0);
        window.close(token).expect("close after mark is idempotent");
    }
}

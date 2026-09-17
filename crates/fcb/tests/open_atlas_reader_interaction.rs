#![forbid(unsafe_code)]

//! Comprehensive test suite for FCB-020.B:
//! Open-atlas-reader-back interaction, gesture ownership, lens dragging versus text selection,
//! side-by-side pinned reading panes, and bounded motion mailbox consumption.

use fcb::{
    ArenaOwnerId, FileId,
    ui::{
        focus::FocusTarget,
        gesture::{
            GestureArbitrator, GestureKind, GestureState, HitRegion, ModifierKeys, PointerButton,
        },
        motion_mailbox::{DiscreteInputEvent, MotionMailbox},
        reducer::{UiAction, UiCommand, UiReducer, UiState},
    },
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(420).unwrap()
}

fn file(id: u64) -> FileId {
    FileId::new(owner(), id).unwrap()
}

#[test]
fn gesture_ownership_locks_out_concurrent_targets() {
    let mut arb = GestureArbitrator::new();
    assert_eq!(arb.state(), GestureState::Idle);

    // Start drag on ReadingLensTitleBar
    let kind = arb.start_pointer_drag(
        HitRegion::ReadingLensTitleBar(1),
        (100.0, 50.0),
        PointerButton::Left,
        ModifierKeys::none(),
        false,
    );
    assert_eq!(kind, Some(GestureKind::LensDrag { pane_id: 1 }));
    assert!(arb.is_active());
    assert_eq!(
        arb.owner_region(),
        Some(HitRegion::ReadingLensTitleBar(1))
    );

    // Negative control: Attempt to start a second gesture on AtlasBackground while active is rejected!
    let second = arb.start_pointer_drag(
        HitRegion::AtlasBackground,
        (300.0, 300.0),
        PointerButton::Left,
        ModifierKeys::none(),
        false,
    );
    assert_eq!(second, None);
    assert_eq!(
        arb.owner_region(),
        Some(HitRegion::ReadingLensTitleBar(1))
    );

    // Pointer move updates owner delta
    let delta = arb.update_pointer_move((120.0, 80.0));
    assert_eq!(
        delta,
        Some((GestureKind::LensDrag { pane_id: 1 }, (20.0, 30.0)))
    );

    // Complete gesture
    let completed = arb.complete_pointer();
    assert_eq!(completed, Some(GestureKind::LensDrag { pane_id: 1 }));
    assert_eq!(arb.state(), GestureState::Idle);
}

#[test]
fn lens_dragging_moves_pane_without_altering_atlas_layout() {
    let mut state = UiState::new(owner());

    // Open reading lens
    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(10),
            path: "crates/fcb/src/lib.rs".to_string(),
            line: Some(1),
        },
        1000,
    );

    let pane_id = state.reading_panes.active_pane_id().unwrap();
    let initial_pos = state.reading_panes.get_pane(pane_id).unwrap().position;
    assert_eq!(initial_pos, (80.0, 60.0));

    // Start drag on title bar
    let out_start = UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::ReadingLensTitleBar(pane_id),
            pos: initial_pos,
            button: PointerButton::Left,
            modifiers: ModifierKeys::none(),
        },
        2000,
    );
    assert!(out_start.changed);
    assert!(state.gesture_arbitrator.is_active());

    // Drag pointer by (+150.0, +100.0)
    let new_pos = (initial_pos.0 + 150.0, initial_pos.1 + 100.0);
    let out_move = UiReducer::reduce(
        &mut state,
        UiAction::PointerMove { pos: new_pos },
        3000,
    );
    assert!(out_move.changed);

    // Pane position updated on screen
    let updated_pane = state.reading_panes.get_pane(pane_id).unwrap();
    assert_eq!(updated_pane.position, (230.0, 160.0));
    assert!(out_move.commands.contains(&UiCommand::ReadingPaneMoved {
        pane_id,
        pos: (230.0, 160.0),
    }));

    // CRITICAL NEGATIVE CONTROL: Zero CameraPan commands emitted! Atlas layout unchanged!
    for cmd in &out_move.commands {
        assert!(
            !matches!(cmd, UiCommand::CameraPan { .. }),
            "Dragging reading lens title bar must NOT pan the atlas camera"
        );
    }

    // Complete drag
    let out_up = UiReducer::reduce(
        &mut state,
        UiAction::PointerUp {
            pos: new_pos,
            button: PointerButton::Left,
        },
        4000,
    );
    assert!(out_up.changed);
    assert!(!state.gesture_arbitrator.is_active());
}

#[test]
fn text_selection_in_lens_body_does_not_move_pane_or_pan_camera() {
    let mut state = UiState::new(owner());

    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(10),
            path: "crates/fcb/src/lib.rs".to_string(),
            line: Some(1),
        },
        1000,
    );

    let pane_id = state.reading_panes.active_pane_id().unwrap();
    let initial_pos = state.reading_panes.get_pane(pane_id).unwrap().position;

    // Pointer down on body (text area)
    let body_pos = (initial_pos.0 + 50.0, initial_pos.1 + 50.0);
    let out_down = UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::ReadingLensBody(pane_id),
            pos: body_pos,
            button: PointerButton::Left,
            modifiers: ModifierKeys::none(),
        },
        2000,
    );
    assert!(out_down.changed);
    assert_eq!(
        state.gesture_arbitrator.current_kind(),
        Some(GestureKind::TextSelect { pane_id })
    );

    // Drag inside body to select text
    let select_pos = (body_pos.0 + 80.0, body_pos.1);
    let out_move = UiReducer::reduce(
        &mut state,
        UiAction::PointerMove { pos: select_pos },
        3000,
    );
    assert!(out_move.changed);
    assert!(out_move.commands.iter().any(|c| matches!(
        c,
        UiCommand::ReadingPaneSelectedText { pane_id: pid, .. } if *pid == pane_id
    )));

    // NEGATIVE CONTROLS: Pane position strictly unchanged; camera strictly unmoved!
    let pane_after = state.reading_panes.get_pane(pane_id).unwrap();
    assert_eq!(pane_after.position, initial_pos);

    for cmd in &out_move.commands {
        assert!(
            !matches!(cmd, UiCommand::CameraPan { .. }),
            "Text selection drag must NOT pan camera"
        );
        assert!(
            !matches!(cmd, UiCommand::ReadingPaneMoved { .. }),
            "Text selection drag must NOT move pane"
        );
    }
}

#[test]
fn scrolling_inside_lens_body_never_pans_atlas() {
    let mut state = UiState::new(owner());

    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(10),
            path: "crates/fcb/src/lib.rs".to_string(),
            line: Some(1),
        },
        1000,
    );

    let pane_id = state.reading_panes.active_pane_id().unwrap();

    // 1. Scroll inside ReadingLensBody
    let out_lens_scroll = UiReducer::reduce(
        &mut state,
        UiAction::ScrollEvent {
            region: HitRegion::ReadingLensBody(pane_id),
            delta: (0.0, 45.0),
            modifiers: ModifierKeys::none(),
        },
        2000,
    );
    assert!(out_lens_scroll.changed);
    assert!(out_lens_scroll.commands.contains(&UiCommand::ReadingPaneScrolled {
        pane_id,
        offset: (0.0, 45.0),
    }));

    // NEGATIVE INVARIANT: Zero CameraPan commands!
    for cmd in &out_lens_scroll.commands {
        assert!(
            !matches!(cmd, UiCommand::CameraPan { .. }),
            "Scroll inside reading lens body must never pan atlas"
        );
    }

    // 2. Contrast: Scroll on AtlasBackground pans atlas!
    let out_atlas_scroll = UiReducer::reduce(
        &mut state,
        UiAction::ScrollEvent {
            region: HitRegion::AtlasBackground,
            delta: (15.0, 30.0),
            modifiers: ModifierKeys::none(),
        },
        3000,
    );
    assert!(out_atlas_scroll.changed);
    assert!(out_atlas_scroll.commands.contains(&UiCommand::CameraPan {
        dx: 15.0,
        dy: 30.0,
    }));
}

#[test]
fn side_by_side_pinned_reading_panes() {
    let mut state = UiState::new(owner());

    // 1. Open file A in pane 1
    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(10),
            path: "crates/a.rs".to_string(),
            line: Some(5),
        },
        1000,
    );
    assert_eq!(state.reading_panes.len(), 1);
    let pane_1 = state.reading_panes.active_pane_id().unwrap();

    // 2. Pin pane 1
    let out_pin = UiReducer::reduce(&mut state, UiAction::PinReadingPane(pane_1), 2000);
    assert!(out_pin.changed);
    assert!(state.reading_panes.get_pane(pane_1).unwrap().is_pinned);
    assert_eq!(state.reading_panes.pinned_count(), 1);

    // 3. Open file B in pane 2 (side-by-side reading)
    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(20),
            path: "crates/b.rs".to_string(),
            line: Some(12),
        },
        3000,
    );
    assert_eq!(state.reading_panes.len(), 2);
    assert_eq!(state.reading_panes.pinned_count(), 1);
    assert_eq!(state.reading_panes.unpinned_count(), 1);
    let pane_2 = state.reading_panes.active_pane_id().unwrap();
    assert_ne!(pane_1, pane_2);

    // 4. Press Escape: unpinned pane 2 closes, pinned pane 1 remains open!
    let out_esc1 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        4000,
    );
    assert!(out_esc1.changed);
    assert_eq!(state.reading_panes.len(), 1);
    assert!(state.reading_panes.get_pane(pane_1).is_some());
    assert!(state.reading_panes.get_pane(pane_2).is_none());
    assert!(state.reading_lens_open);

    // 5. Unpin pane 1 and close it
    UiReducer::reduce(&mut state, UiAction::UnpinReadingPane(pane_1), 5000);
    assert_eq!(state.reading_panes.pinned_count(), 0);

    let out_esc2 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        6000,
    );
    assert!(out_esc2.changed);
    assert_eq!(state.reading_panes.len(), 0);
    assert!(!state.reading_lens_open);
}

#[test]
fn city_mode_orbit_requires_deliberate_modifier() {
    let mut state = UiState::new(owner());

    // Enable city mode
    let out_city = UiReducer::reduce(&mut state, UiAction::SetCityMode(true), 1000);
    assert!(out_city.changed);
    assert!(state.is_city_mode);

    // Negative control: Left-drag on Atlas without modifier triggers CameraPan, NOT Orbit!
    let out_pan = UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::AtlasBackground,
            pos: (200.0, 200.0),
            button: PointerButton::Left,
            modifiers: ModifierKeys::none(),
        },
        2000,
    );
    assert!(out_pan.changed);
    assert_eq!(
        state.gesture_arbitrator.current_kind(),
        Some(GestureKind::CameraPan)
    );

    UiReducer::reduce(
        &mut state,
        UiAction::PointerUp {
            pos: (200.0, 200.0),
            button: PointerButton::Left,
        },
        3000,
    );

    // Positive case: Left-drag on Atlas WITH Alt modifier triggers CameraOrbit!
    let out_orbit = UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::AtlasBackground,
            pos: (200.0, 200.0),
            button: PointerButton::Left,
            modifiers: ModifierKeys::alt(),
        },
        4000,
    );
    assert!(out_orbit.changed);
    assert_eq!(
        state.gesture_arbitrator.current_kind(),
        Some(GestureKind::CameraOrbit)
    );

    let out_move = UiReducer::reduce(
        &mut state,
        UiAction::PointerMove { pos: (220.0, 210.0) },
        5000,
    );
    assert!(out_move.commands.contains(&UiCommand::CameraOrbit {
        dx: 20.0,
        dy: 10.0,
    }));
}

#[test]
fn escape_cancels_active_gesture_before_closing_lens_or_changing_scope() {
    let mut state = UiState::new(owner());

    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(10),
            path: "crates/fcb/src/lib.rs".to_string(),
            line: None,
        },
        1000,
    );
    assert!(state.reading_lens_open);

    let pane_id = state.reading_panes.active_pane_id().unwrap();

    // Start a drag gesture
    UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::ReadingLensTitleBar(pane_id),
            pos: (100.0, 100.0),
            button: PointerButton::Left,
            modifiers: ModifierKeys::none(),
        },
        2000,
    );
    assert!(state.gesture_arbitrator.is_active());

    // Press Escape while gesture is active
    let out_esc1 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        3000,
    );
    assert!(out_esc1.changed);
    assert!(!state.gesture_arbitrator.is_active());

    // INVARIANT: Gesture is cancelled, but reading lens remains OPEN!
    assert!(state.reading_lens_open);
    assert_eq!(state.reading_panes.len(), 1);

    // Second Escape closes the lens
    let out_esc2 = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        4000,
    );
    assert!(out_esc2.changed);
    assert!(!state.reading_lens_open);
    assert_eq!(state.reading_panes.len(), 0);
}

#[test]
fn motion_mailbox_coalesces_continuous_motion_and_preserves_ordered_discrete_events() {
    let mut mailbox = MotionMailbox::new(64);

    // Enqueue 4 discrete events
    mailbox.push_discrete(DiscreteInputEvent::PointerDown {
        region: HitRegion::AtlasBackground,
        pos: (50.0, 50.0),
        button: PointerButton::Left,
        modifiers: ModifierKeys::none(),
    });
    mailbox.push_discrete(DiscreteInputEvent::FocusTargetSelected(FocusTarget::Atlas));
    mailbox.push_discrete(DiscreteInputEvent::KeyDown {
        key: "Enter".to_string(),
        modifiers: ModifierKeys::none(),
    });
    mailbox.push_discrete(DiscreteInputEvent::PointerUp {
        pos: (50.0, 50.0),
        button: PointerButton::Left,
    });

    // Simultaneously record 40 high-frequency mouse move deltas and 10 scroll ticks
    for _ in 0..40 {
        mailbox.record_move((60.0, 60.0), (1.5, 0.5));
    }
    for _ in 0..10 {
        mailbox.record_scroll((0.0, 2.0));
    }
    mailbox.record_pinch(0.05);

    // Continuous motion is coalesced into latest-state mailbox
    let motion = mailbox.take_motion().expect("motion must be present");
    assert!((motion.pan_delta.0 - 60.0).abs() < 1e-4);
    assert!((motion.pan_delta.1 - 20.0).abs() < 1e-4);
    assert!((motion.scroll_delta.1 - 20.0).abs() < 1e-4);
    assert!((motion.pinch_magnification - 0.05).abs() < 1e-4);
    assert_eq!(motion.latest_pointer, Some((60.0, 60.0)));
    assert_eq!(motion.coalesced_count, 51);

    // Accumulators are reset after take_motion()
    assert_eq!(mailbox.take_motion(), None);

    // Discrete events retain strict FIFO order!
    let discrete_batch = mailbox.drain_discrete_batch(10);
    assert_eq!(discrete_batch.len(), 4);
    assert!(matches!(
        discrete_batch[0],
        DiscreteInputEvent::PointerDown { .. }
    ));
    assert!(matches!(
        discrete_batch[1],
        DiscreteInputEvent::FocusTargetSelected(FocusTarget::Atlas)
    ));
    assert!(matches!(
        discrete_batch[2],
        DiscreteInputEvent::KeyDown { .. }
    ));
    assert!(matches!(
        discrete_batch[3],
        DiscreteInputEvent::PointerUp { .. }
    ));
}

#[test]
fn bounded_delta_consumption_enforces_backpressure_and_frame_budgets() {
    let mut mailbox = MotionMailbox::new(32);

    // Push 50 discrete events into capacity 32 queue
    for i in 0..50 {
        mailbox.push_discrete(DiscreteInputEvent::KeyDown {
            key: format!("Key{i}"),
            modifiers: ModifierKeys::none(),
        });
    }

    // Queue is bounded to 32, 18 events were dropped
    assert_eq!(mailbox.discrete_len(), 32);
    assert_eq!(mailbox.dropped_discrete_count(), 18);

    // Frame consumption is bounded: requesting batch of 8 returns at most 8
    let batch1 = mailbox.drain_discrete_batch(8);
    assert_eq!(batch1.len(), 8);
    assert_eq!(mailbox.discrete_len(), 24);

    let batch2 = mailbox.drain_discrete_batch(8);
    assert_eq!(batch2.len(), 8);
    assert_eq!(mailbox.discrete_len(), 16);
}

#[test]
fn real_repository_journey_open_atlas_reader_back() {
    let mut state = UiState::new(owner());
    assert_eq!(state.focus.current(), FocusTarget::Atlas);

    // 1. Select parcel in atlas
    let out_sel = UiReducer::reduce(
        &mut state,
        UiAction::SelectFile {
            file_id: file(42),
            path: "crates/fcb/src/lib.rs".to_string(),
        },
        1000,
    );
    assert!(out_sel.changed);
    assert_eq!(state.focus.current(), FocusTarget::Atlas); // Focus is NOT stolen by selection
    assert_eq!(
        state.selected_file,
        Some((file(42), "crates/fcb/src/lib.rs".to_string()))
    );

    // 2. User presses "Enter" on atlas -> promotes selected parcel to ReadingLens!
    let out_enter = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Enter".to_string()),
        2000,
    );
    assert!(out_enter.changed);
    assert!(state.reading_lens_open);
    assert_eq!(state.focus.current(), FocusTarget::ReadingLens);
    let pane_1 = state.reading_panes.active_pane_id().unwrap();

    // 3. Drag reading lens pane title bar to new floating position
    UiReducer::reduce(
        &mut state,
        UiAction::PointerDown {
            region: HitRegion::ReadingLensTitleBar(pane_1),
            pos: (80.0, 60.0),
            button: PointerButton::Left,
            modifiers: ModifierKeys::none(),
        },
        3000,
    );
    UiReducer::reduce(
        &mut state,
        UiAction::PointerMove { pos: (200.0, 150.0) },
        3500,
    );
    UiReducer::reduce(
        &mut state,
        UiAction::PointerUp {
            pos: (200.0, 150.0),
            button: PointerButton::Left,
        },
        4000,
    );
    assert_eq!(
        state.reading_panes.get_pane(pane_1).unwrap().position,
        (200.0, 150.0)
    );

    // 4. Pin pane 1
    UiReducer::reduce(&mut state, UiAction::PinReadingPane(pane_1), 5000);
    assert!(state.reading_panes.get_pane(pane_1).unwrap().is_pinned);

    // 5. Open second file (crates/fcb/src/ui.rs)
    UiReducer::reduce(
        &mut state,
        UiAction::OpenReadingLens {
            file_id: file(43),
            path: "crates/fcb/src/ui.rs".to_string(),
            line: Some(10),
        },
        6000,
    );
    assert_eq!(state.reading_panes.len(), 2);
    let pane_2 = state.reading_panes.active_pane_id().unwrap();
    assert_ne!(pane_1, pane_2);

    // 6. Escape closes the unpinned pane 2
    UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        7000,
    );
    assert_eq!(state.reading_panes.len(), 1);
    assert!(state.reading_panes.get_pane(pane_1).is_some());
    assert!(state.reading_panes.get_pane(pane_2).is_none());

    // 7. Unpin pane 1 and Escape returns focus to Atlas
    UiReducer::reduce(&mut state, UiAction::UnpinReadingPane(pane_1), 8000);
    let out_final_esc = UiReducer::reduce(
        &mut state,
        UiAction::KeyboardShortcut("Escape".to_string()),
        9000,
    );
    assert!(out_final_esc.changed);
    assert!(!state.reading_lens_open);
    assert_eq!(state.reading_panes.len(), 0);
    assert_eq!(state.focus.current(), FocusTarget::Atlas);

    // Bounded event ring contains complete sequence without overflow
    assert!(state.event_ring.len() <= 256);
}

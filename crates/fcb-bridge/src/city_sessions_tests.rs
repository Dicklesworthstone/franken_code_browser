use super::*;

fn input() -> CityInput<'static> {
    CityInput { ids: &[91], rects: &[0.0, 0.0, 100.0, 100.0], observed_bytes: &[1024], known: &[1] }
}
fn view(generation: u64) -> Command {
    Command::Prepare { generation, action: CityViewAction::View { width: 100.0, height: 100.0,
        center_x: 50.0, center_y: 50.0, scale: 1.0, yaw: 0.0, tilt: 0.0 } }
}
fn pick() -> Command { Command::Pick { frame: 1, display: 1, x: 50.0, y: 50.0 } }

#[test]
fn independently_owned_city_handles_cannot_borrow_each_others_frames() {
    let sessions = CitySessions::new();
    let a = sessions.create().unwrap(); let b = sessions.create().unwrap();
    sessions.open(a, 1, input(), true, || false).unwrap();
    sessions.open(b, 1, input(), true, || false).unwrap();
    sessions.execute(a, view(1), || false).unwrap();
    sessions.execute(a, Command::Present { generation: 1, frame: 1, display: 1 }, || false).unwrap();
    assert!(matches!(sessions.execute(b, pick(), || false), Err(AccessError::City(CitySessionError::MissingView))));
    let result = sessions.execute(a, pick(), || false).unwrap();
    assert!(result.as_str().contains("\"file_id\":\"91\""));
    sessions.close(b).unwrap();
    assert!(sessions.execute(a, pick(), || false).is_ok());
}

#[test]
fn canceled_open_keeps_handle_empty_and_retryable() {
    let sessions = CitySessions::new(); let handle = sessions.create().unwrap();
    assert!(sessions.open(handle, 1, input(), true, || true).is_err());
    assert!(matches!(sessions.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    sessions.open(handle, 1, input(), true, || false).unwrap();
    assert!(matches!(sessions.open(handle, 2, input(), true, || false), Err(AccessError::AlreadyOpen)));
    assert!(sessions.execute(handle, Command::Info, || false).unwrap().as_str().contains("\"layout\":\"1\""));
}

#[test]
fn cancel_between_calls_invalidates_unpresented_view_but_keeps_visible_view() {
    let sessions = CitySessions::new(); let handle = sessions.create().unwrap();
    sessions.open(handle, 1, input(), true, || false).unwrap();
    sessions.execute(handle, view(1), || false).unwrap();
    sessions.execute(handle, Command::Present { generation: 1, frame: 1, display: 1 }, || false).unwrap();
    sessions.execute(handle, view(2), || false).unwrap(); sessions.cancel(handle).unwrap();
    assert!(matches!(sessions.execute(handle, Command::Present { generation: 2, frame: 2, display: 1 }, || false),
        Err(AccessError::City(CitySessionError::MissingView))));
    assert!(sessions.execute(handle, pick(), || false).is_ok());
    sessions.execute(handle, view(3), || false).unwrap();
}

#[test]
fn closed_inflight_scene_keeps_capacity_until_call_retires() {
    let sessions = CitySessions::new();
    let handles: Vec<_> = (0..MAX_CITY_SESSIONS).map(|_| sessions.create().unwrap()).collect();
    let handle = handles[0]; sessions.open(handle, 1, input(), true, || false).unwrap();
    assert!(matches!(sessions.create(), Err(AccessError::Capacity)));
    let mut closed = false;
    let result = sessions.execute(handle, view(1), || {
        if !closed {
            closed = true; sessions.close(handle).unwrap();
            assert!(matches!(sessions.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    let replacement = sessions.create().unwrap(); assert_ne!(replacement, handle);
    assert!(matches!(sessions.execute(handle, Command::Info, || false), Err(AccessError::Unknown)));
}

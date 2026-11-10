use super::*;

fn session() -> CitySession {
    CitySession::open(ArenaOwnerId::new(7).unwrap(), 3, CityInput {
        ids: &[11, 22], rects: &[0.0, 0.0, 100.0, 100.0, 200.0, 0.0, 100.0, 100.0],
        observed_bytes: &[1024, 2048], known: &[1, 1],
    }, true, || false).unwrap()
}
fn view() -> CityViewAction {
    CityViewAction::View { width: 300.0, height: 100.0, center_x: 150.0,
        center_y: 50.0, scale: 1.0, yaw: 0.0, tilt: 0.0 }
}
fn hit(response: HostResponse, id: u64) {
    assert!(response.as_str().contains(&format!("\"file_id\":\"{id}\"")), "{}", response.as_str());
    assert!(response.as_str().contains("\"picking\":\"frontmost-cuboid-not-source-location\""));
}

#[test]
fn prepared_camera_never_replaces_displayed_picking() {
    let mut city = session();
    city.prepare(1, view(), || false).unwrap();
    assert!(matches!(city.pick(1, 1, 50.0, 50.0, || false), Err(CitySessionError::MissingView)));
    city.present(1, 1, 1, || false).unwrap();
    hit(city.pick(1, 1, 50.0, 50.0, || false).unwrap(), 11);
    city.prepare(2, CityViewAction::Pan { base: 1, dx: 1000.0, dy: 0.0 }, || false).unwrap();
    hit(city.pick(1, 1, 50.0, 50.0, || false).unwrap(), 11);
    city.present(2, 2, 1, || false).unwrap();
    assert!(city.pick(2, 1, 50.0, 50.0, || false).unwrap().as_str().contains("\"file_id\":null"));
    assert!(matches!(city.pick(1, 1, 50.0, 50.0, || false), Err(CitySessionError::StaleFrame)));
    assert!(matches!(city.pick(2, 2, 50.0, 50.0, || false), Err(CitySessionError::StaleFrame)));
}

#[test]
fn failed_attempt_consumes_generation_and_cannot_change_selection() {
    let mut city = session();
    city.prepare(1, view(), || false).unwrap();
    city.present(1, 1, 1, || false).unwrap();
    assert!(city.prepare(2, CityViewAction::Zoom { base: 1, x: 50.0, y: 50.0, factor: f64::NAN }, || false).is_err());
    assert!(matches!(city.prepare(2, view(), || false), Err(CitySessionError::StaleGeneration)));
    assert!(matches!(city.present(2, 2, 1, || false), Err(CitySessionError::MissingView)));
    hit(city.pick(1, 1, 50.0, 50.0, || false).unwrap(), 11);
}

#[test]
fn acknowledgement_is_idempotent_but_older_views_cannot_replace_newer_frames() {
    let mut city = session();
    city.prepare(1, view(), || false).unwrap();
    city.prepare(2, view(), || false).unwrap();
    city.present(1, 10, 4, || false).unwrap();
    let first = city.present(2, 11, 4, || false).unwrap();
    let replay = city.present(2, 11, 4, || false).unwrap();
    assert_eq!(first.as_str(), replay.as_str());
    assert!(matches!(city.present(1, 12, 4, || false), Err(CitySessionError::StaleFrame)));
    assert!(matches!(city.present(2, 12, 3, || false), Err(CitySessionError::StaleFrame)));
    assert!(matches!(city.present(2, 10, 5, || false), Err(CitySessionError::StaleFrame)));
    hit(city.pick(11, 4, 250.0, 50.0, || false).unwrap(), 22);
}

#[test]
fn retained_candidate_ring_is_bounded_and_keeps_actual_presentation() {
    let mut city = session();
    city.prepare(1, view(), || false).unwrap();
    city.present(1, 1, 1, || false).unwrap();
    for generation in 2..=20 { city.prepare(generation, view(), || false).unwrap(); }
    assert_eq!(city.pending.iter().flatten().count(), MAX_CITY_PENDING_VIEWS);
    assert!(matches!(city.present(2, 2, 1, || false), Err(CitySessionError::MissingView)));
    // A style-only redraw of the still-visible generation is valid even after
    // its prepared slot has been evicted; it does not invent another camera.
    city.present(1, 2, 1, || false).unwrap();
    hit(city.pick(2, 1, 50.0, 50.0, || false).unwrap(), 11);
    city.present(20, 3, 1, || false).unwrap();
}

#[test]
fn cancellation_revokes_candidates_and_preserves_accepted_frame() {
    let mut city = session();
    city.prepare(1, view(), || false).unwrap(); city.present(1, 1, 1, || false).unwrap();
    city.prepare(2, view(), || false).unwrap(); city.cancel_pending();
    assert!(matches!(city.present(2, 2, 1, || false), Err(CitySessionError::MissingView)));
    hit(city.pick(1, 1, 50.0, 50.0, || false).unwrap(), 11);
    assert!(matches!(city.prepare(3, view(), || true), Err(CitySessionError::Canceled)));
    assert!(matches!(city.prepare(3, view(), || false), Err(CitySessionError::StaleGeneration)));
    city.prepare(4, CityViewAction::Orbit { base: 1, yaw: 0.2, tilt: 0.3 }, || false).unwrap();
}

#[test]
fn scene_reports_unknown_empty_capped_and_partial_observations_without_io() {
    let large_id = (1u64 << 53) + 1;
    let mut city = CitySession::open(ArenaOwnerId::new(8).unwrap(), 9, CityInput {
        ids: &[large_id, large_id + 1, large_id + 2],
        rects: &[0.0, 0.0, 10.0, 10.0, 20.0, 0.0, 10.0, 10.0, 40.0, 0.0, 10.0, 10.0],
        observed_bytes: &[0, u64::MAX, u64::MAX], known: &[1, 0, 1],
    }, false, || false).unwrap();
    let response = city.info(|| false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    for field in ["\"unknown_count\":\"1\"", "\"capped_count\":\"1\"", "\"observed_bytes\":null",
        "\"observed_bytes\":\"0\"", "\"source_payload_read\":false", "\"discovery_complete\":false"] {
        assert!(response.as_str().contains(field), "{field}: {}", response.as_str());
    }
    assert!(response.as_str().contains(&format!("\"file_id\":\"{large_id}\"")));
    assert_eq!(city.info(|| false).unwrap().as_str(), response.as_str());
}

#[test]
fn invalid_arrays_refused_before_scene_and_empty_scene_can_be_fitted() {
    let owner = ArenaOwnerId::new(10).unwrap();
    let bad = CityInput { ids: &[1], rects: &[], observed_bytes: &[0], known: &[1] };
    assert!(matches!(CitySession::open(owner, 1, bad, true, || false), Err(CitySessionError::InvalidInput)));
    let bad = CityInput { ids: &[1], rects: &[0.0, 0.0, 10.0, 10.0], observed_bytes: &[0], known: &[2] };
    assert!(matches!(CitySession::open(owner, 1, bad, true, || false), Err(CitySessionError::InvalidInput)));
    let mut empty = CitySession::open(owner, 1, CityInput { ids: &[], rects: &[], observed_bytes: &[], known: &[] }, true, || false).unwrap();
    empty.prepare(1, CityViewAction::Fit { width: 800.0, height: 600.0 }, || false).unwrap();
    empty.present(1, 1, 1, || false).unwrap();
    assert!(empty.pick(1, 1, 400.0, 300.0, || false).unwrap().as_str().contains("\"file_id\":null"));
}

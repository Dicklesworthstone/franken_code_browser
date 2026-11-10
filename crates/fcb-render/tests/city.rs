#![forbid(unsafe_code)]

//! Production geometry and camera tests. These exercise the shared CPU engine;
//! they do not claim Metal raster or physical-Mac qualification.

use fcb_core::{ArenaOwnerId, ByteLength, LayoutRevision, PresentedFrameId, Rect2D,
    ResourceAllocationId, ResourceBudget};
use fcb_render::city::{Camera, CityError, Frame, Record, Scene, MAX_CITY_RECORDS,
    MAX_TILT_RADIANS, METRIC_CAP_BYTES};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(71).unwrap() }
fn layout(value: u64) -> LayoutRevision { LayoutRevision::new(owner(), value).unwrap() }
fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
fn frame_id(value: u64) -> PresentedFrameId { PresentedFrameId::new(owner(), value).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
fn record(file_id: u64, x: f64, y: f64, width: f64, height: f64, bytes: Option<u64>) -> Record {
    Record { file_id, footprint: Rect2D::from_xywh(x, y, width, height).unwrap(), observed_bytes: bytes }
}
fn scene(records: &[Record], budget: &ResourceBudget) -> Scene {
    Scene::new(layout(1), records, budget, allocation(1), || false).unwrap()
}
fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!((actual - expected).abs() <= tolerance, "{actual} differs from {expected}");
}
fn projected(frame: &Frame, point: [f64; 3]) -> [f64; 2] {
    let point = frame.project(point).unwrap(); [point[0], point[1]]
}

#[test]
fn top_down_matrix_preserves_atlas_orientation_and_quarter_turn() {
    let budget = budget();
    let scene = scene(&[record(1, 0.0, 0.0, 100.0, 80.0, None)], &budget);
    let camera = Camera::new(800.0, 600.0, 50.0, 40.0, 2.0, 0.0, 0.0).unwrap();
    let frame = scene.frame(frame_id(1), camera).unwrap();
    let point = frame.project([60.0, 55.0, 0.0]).unwrap();
    close(point[0], 420.0, 0.0001); close(point[1], 330.0, 0.0001);
    assert!((0.0..=1.0).contains(&point[2]));
    let rotated = scene.frame(frame_id(2), camera.orbit(std::f64::consts::FRAC_PI_2, 0.0).unwrap()).unwrap();
    let point = rotated.project([60.0, 55.0, 0.0]).unwrap();
    close(point[0], 370.0, 0.0001); close(point[1], 320.0, 0.0001);
    assert_eq!(scene.pick(&rotated, point[0], point[1]).unwrap().unwrap().file_id(), 1);
    let matrix = frame.matrix();
    assert_eq!([matrix[3], matrix[7], matrix[11], matrix[15]], [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn tilted_tall_foreground_occludes_a_shorter_file_and_orbit_reverses_it() {
    let budget = budget();
    let scene = scene(&[
        record(10, 0.0, 0.0, 4.0, 4.0, Some(0)),
        record(20, 0.0, 8.0, 4.0, 4.0, Some(METRIC_CAP_BYTES)),
        // Establish a 100-unit scene width; the metric ceiling is 12 units.
        record(30, 99.0, 0.0, 1.0, 1.0, None),
    ], &budget);
    let short_height = f64::from(scene.instances()[0].height());
    close(f64::from(scene.instances()[1].height()), 12.0, 0.0001);
    let camera = Camera::new(600.0, 600.0, 2.0, 4.0, 10.0, 0.0, std::f64::consts::FRAC_PI_4).unwrap();
    let frame = scene.frame(frame_id(1), camera).unwrap();
    let target = projected(&frame, [2.0, 2.0, short_height]);
    assert_eq!(scene.pick(&frame, target[0], target[1]).unwrap().unwrap().file_id(), 20,
        "picking the back file's projected interior must stop at the visible foreground surface");
    let reversed = scene.frame(frame_id(2), camera.orbit(std::f64::consts::PI, 0.0).unwrap()).unwrap();
    let target = projected(&reversed, [2.0, 2.0, short_height]);
    assert_eq!(scene.pick(&reversed, target[0], target[1]).unwrap().unwrap().file_id(), 10);
}

#[test]
fn unknowns_are_pickable_flat_planes_and_equal_depth_uses_lower_file_id() {
    let budget = budget();
    let scene = scene(&[record(9, 0.0, 0.0, 10.0, 10.0, None),
        record(3, 0.0, 0.0, 10.0, 10.0, None)], &budget);
    assert_eq!(scene.stats().unknown_files, 2);
    assert_eq!(scene.stats().known_files, 0);
    assert_eq!(scene.instances().iter().map(|i| i.file_id()).collect::<Vec<_>>(), [3, 9]);
    assert!(scene.instances().iter().all(|i| i.height() == 0.0));
    for (index, yaw) in [0.0, 1.1, 3.8].into_iter().enumerate() {
        let camera = Camera::new(400.0, 300.0, 5.0, 5.0, 10.0, yaw, 0.7).unwrap();
        let frame = scene.frame(frame_id(index as u64 + 1), camera).unwrap();
        assert_eq!(scene.pick(&frame, 200.0, 150.0).unwrap().unwrap().file_id(), 3);
        assert!(scene.pick(&frame, -1.0, 150.0).unwrap().is_none());
        assert!(scene.pick(&frame, 400.0, 150.0).unwrap().is_none());
        assert_eq!(scene.pick(&frame, f64::NAN, 0.0), Err(CityError::InvalidPoint));
    }
}

#[test]
fn canonical_rebased_instances_fit_with_heights_and_preserve_original_footprints() {
    let budget = budget();
    let records = [record(5, 1.0e12, -1.0e12, 128.0, 32.0, Some(METRIC_CAP_BYTES + 1)),
        record(2, 1.0e12 + 160.0, -1.0e12 + 80.0, 16.0, 64.0, Some(0))];
    let scene = scene(&records, &budget);
    assert_eq!(scene.stats().capped_files, 1);
    assert_eq!(scene.stats().bvh_nodes, 3);
    let camera = scene.fit_camera(800.0, 600.0, 24.0).unwrap();
    let frame = scene.frame(frame_id(1), camera).unwrap();
    let origin = scene.origin();
    for instance in scene.instances() {
        assert_eq!(instance.footprint(), records.iter().find(|r| r.file_id == instance.file_id()).unwrap().footprint);
        let [x, y, width, height] = instance.rect();
        for px in [x, x + width] {
            for py in [y, y + height] {
                for z in [0.0, instance.height()] {
                    let p = frame.project([origin[0] + f64::from(px), origin[1] + f64::from(py), f64::from(z)]).unwrap();
                    assert!((23.99..=776.01).contains(&p[0]), "x outside fitted scene: {p:?}");
                    assert!((23.99..=576.01).contains(&p[1]), "y outside fitted scene: {p:?}");
                    assert!((0.0..=1.0).contains(&p[2]));
                }
            }
        }
    }
}

#[test]
fn gestures_preserve_ground_anchor_and_resizing_preserves_center() {
    let budget = budget();
    let scene = scene(&[record(1, -100.0, -100.0, 200.0, 200.0, None)], &budget);
    let camera = Camera::new(800.0, 600.0, 5.0, 6.0, 2.0, 0.6, 0.7).unwrap();
    let point = [17.0, 19.0, 0.0];
    let first = scene.frame(frame_id(1), camera).unwrap();
    let original = projected(&first, point);
    let moved = scene.frame(frame_id(2), camera.pan(31.0, -17.0).unwrap()).unwrap();
    let after_pan = projected(&moved, point);
    close(after_pan[0], original[0] + 31.0, 0.001);
    close(after_pan[1], original[1] - 17.0, 0.001);
    let zoom = camera.zoom_at(original[0], original[1], 3.5).unwrap();
    let zoomed = scene.frame(frame_id(3), zoom).unwrap();
    let after_zoom = projected(&zoomed, point);
    close(after_zoom[0], original[0], 0.001); close(after_zoom[1], original[1], 0.001);
    let resized = zoom.with_viewport(1200.0, 900.0).unwrap();
    assert_eq!(resized.center(), zoom.center()); assert_eq!(resized.scale(), zoom.scale());
    assert_eq!(resized.orbit(100.0, 100.0).unwrap().tilt(), MAX_TILT_RADIANS);
    assert_eq!(resized.orbit(-100.0, -100.0).unwrap().tilt(), 0.0);
    assert_eq!(camera.zoom_at(0.0, 0.0, 0.0), Err(CityError::InvalidCamera));
    assert_eq!(camera.pan(f64::INFINITY, 0.0), Err(CityError::InvalidCamera));
}

#[test]
fn admission_cancellation_and_live_frame_leases_preserve_accounting() {
    let records = [record(1, 0.0, 0.0, 10.0, 10.0, Some(1))];
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(Scene::new(layout(1), &records, &tiny, allocation(1), || false), Err(CityError::ResourceDenied)));
    assert_eq!(tiny.accounting().managed().get(), 0);
    let budget = budget();
    let mut calls = 0;
    assert!(matches!(Scene::new(layout(1), &records, &budget, allocation(1), || {
        calls += 1; calls > records.len() + 1
    }), Err(CityError::Canceled)));
    assert_eq!(budget.accounting().managed().get(), 0);
    assert!(budget.accounting().peak_reserved().get() > 0, "cancellation exercised admitted construction");
    let first = scene(&records, &budget);
    let charge = first.stats().managed_bytes;
    let frame = first.frame(frame_id(1), first.fit_camera(400.0, 300.0, 12.0).unwrap()).unwrap();
    drop(first);
    assert_eq!(budget.accounting().managed().get(), charge);
    assert!(matches!(Scene::new(layout(2), &records, &budget, allocation(1), || false), Err(CityError::ResourceDenied)));
    drop(frame);
    assert_eq!(budget.accounting().managed().get(), 0);
    let replacement = Scene::new(layout(2), &records, &budget, allocation(1), || false).unwrap();
    assert_eq!(replacement.stats().managed_bytes, charge);
}

#[test]
fn invalid_inputs_empty_scene_and_stale_frame_fail_explicitly() {
    let budget = budget();
    let empty = scene(&[], &budget);
    assert_eq!(empty.bounds(), None); assert_eq!(empty.stats().records, 0);
    let frame = empty.frame(frame_id(1), empty.fit_camera(400.0, 300.0, 12.0).unwrap()).unwrap();
    assert!(empty.pick(&frame, 200.0, 150.0).unwrap().is_none());
    assert!(Camera::new(0.0, 300.0, 0.0, 0.0, 1.0, 0.0, 0.0).is_err());
    assert!(Camera::new(400.0, 300.0, 0.0, 0.0, 1.0, f64::NAN, 0.0).is_err());
    assert!(Camera::new(400.0, 300.0, 0.0, 0.0, 1.0, 0.0, MAX_TILT_RADIANS + 0.1).is_err());
    let one = record(1, 0.0, 0.0, 10.0, 10.0, None);
    let duplicate = Scene::new(layout(2), &[one, one], &budget, allocation(2), || false);
    assert!(matches!(duplicate, Err(CityError::DuplicateFile)));
    let excessive = vec![one; MAX_CITY_RECORDS + 1];
    assert!(matches!(Scene::new(layout(2), &excessive, &budget, allocation(2), || false), Err(CityError::TooManyRecords)));
    let huge = record(2, 1.0e300, 1.0e300, 1.0e300, 1.0e300, None);
    assert!(matches!(Scene::new(layout(2), &[huge], &budget, allocation(2), || false), Err(CityError::PrecisionLost)));
    let replacement = Scene::new(layout(2), &[one], &budget, allocation(2), || false).unwrap();
    assert_eq!(replacement.pick(&frame, 200.0, 150.0), Err(CityError::StaleFrame));
    let other = ArenaOwnerId::new(72).unwrap();
    assert!(matches!(replacement.frame(PresentedFrameId::new(other, 1).unwrap(), frame.camera()), Err(CityError::OwnerMismatch)));
}

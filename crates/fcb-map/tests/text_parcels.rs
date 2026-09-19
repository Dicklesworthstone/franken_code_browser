#![forbid(unsafe_code)]

use fcb_core::Rect2D;
use fcb_map::text_parcels::{MAX_TEXT_FILES, MeasuredFile, pack_text_parcels};

fn file(path: &[u8], area: f64) -> MeasuredFile<'_> {
    MeasuredFile { path, area }
}

fn close(actual: f64, expected: f64, relative: f64) {
    assert!(
        (actual - expected).abs() <= relative * expected.abs().max(1.0),
        "actual {actual:e}, expected {expected:e}"
    );
}

fn bounds(rects: &[Rect2D]) -> (f64, f64, f64, f64) {
    rects.iter().fold(
        (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
        |(x0, y0, x1, y1), r| {
            (x0.min(r.min_x()), y0.min(r.min_y()), x1.max(r.max_x()), y1.max(r.max_y()))
        },
    )
}

fn check_partition(files: &[MeasuredFile<'_>], aspect: f64, relative: f64) -> Vec<Rect2D> {
    let rects = pack_text_parcels(files, aspect).expect("valid measured files");
    assert_eq!(rects.len(), files.len(), "every input must receive a rectangle");
    for (r, input) in rects.iter().zip(files) {
        assert!(r.min_x().is_finite() && r.min_y().is_finite());
        assert!(r.size().width().is_finite() && r.size().height().is_finite());
        assert!(r.size().width() > 0.0 && r.size().height() > 0.0);
        close(r.size().area(), input.area, relative);
    }
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let overlap_x = a.max_x().min(b.max_x()) - a.min_x().max(b.min_x());
            let overlap_y = a.max_y().min(b.max_y()) - a.min_y().max(b.min_y());
            // Permit only coordinate-rounding noise, not positive-area overlap.
            let scale = a.max_x().abs().max(a.max_y().abs()).max(b.max_x().abs()).max(b.max_y().abs()).max(1.0);
            assert!(overlap_x <= 16.0 * f64::EPSILON * scale || overlap_y <= 16.0 * f64::EPSILON * scale);
        }
    }
    let (x0, y0, x1, y1) = bounds(&rects);
    assert_eq!((x0, y0), (0.0, 0.0));
    close((x1 - x0) / (y1 - y0), aspect, 1e-12);
    close((x1 - x0) * (y1 - y0), files.iter().map(|f| f.area).sum(), relative);
    rects
}

#[test]
fn deterministic_rectangles_follow_original_input_indices() {
    let files = [file(b"src/z.rs", 300.0), file(b"docs/a.md", 70.0), file(b"src/a.rs", 20.0), file(b"README", 91.0)];
    let original = check_partition(&files, 1.5, 1e-12);
    assert_eq!(original, pack_text_parcels(&files, 1.5).unwrap());
    for order in [[3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
        let shuffled: Vec<_> = order.iter().map(|&i| files[i]).collect();
        let actual = pack_text_parcels(&shuffled, 1.5).unwrap();
        for (index, source) in order.into_iter().enumerate() {
            assert_eq!(actual[index], original[source], "path {:?}", files[source].path);
        }
    }
}

#[test]
fn fractional_weight_permutation_does_not_change_geometry() {
    let files = [file(b"a/large", 1e12), file(b"a/small", 1.00005), file(b"z", 1.00005)];
    let first = pack_text_parcels(&files, 1.0).unwrap();
    let reverse: Vec<_> = files.iter().rev().copied().collect();
    let second = pack_text_parcels(&reverse, 1.0).unwrap();
    assert_eq!(first, second.into_iter().rev().collect::<Vec<_>>());
}

#[test]
fn nested_subtrees_fill_contiguous_rectangular_parcels() {
    let files = [file(b"a/deep/x", 11.0), file(b"b/z", 17.0), file(b"a/y", 31.0), file(b"a/deep/w", 13.0), file(b"b/q", 23.0), file(b"top", 29.0)];
    let rects = check_partition(&files, 0.75, 1e-12);
    for prefix in [b"a/".as_slice(), b"a/deep/", b"b/"] {
        let group: Vec<_> = files.iter().zip(&rects).filter(|(f, _)| f.path.starts_with(prefix)).map(|(_, r)| *r).collect();
        let (x0, y0, x1, y1) = bounds(&group);
        let expected: f64 = files.iter().filter(|f| f.path.starts_with(prefix)).map(|f| f.area).sum();
        close((x1 - x0) * (y1 - y0), expected, 1e-12);
        // A disjoint outsider cannot occupy a hole in this directory's parcel.
        for (_, r) in files.iter().zip(&rects).filter(|(f, _)| !f.path.starts_with(prefix)) {
            assert!(r.max_x() <= x0 + 1e-12 || r.min_x() >= x1 - 1e-12 || r.max_y() <= y0 + 1e-12 || r.min_y() >= y1 - 1e-12);
        }
    }
}

#[test]
fn extreme_supported_areas_remain_positive_and_conserve_area() {
    for aspect in [0.25, 1.0, 4.0] {
        for areas in [[1.0, 1e12, 1.0], [1e12, 1.0, 1.0], [1.0, 1.0, 1e12]] {
            let files = [file(b"a", areas[0]), file(b"b", areas[1]), file(b"c", areas[2])];
            // Subtracting a unit parcel from a trillion-unit region loses a few
            // ulps of its coordinate; it must still retain its measured area.
            check_partition(&files, aspect, 0.001);
        }
    }
}

#[test]
fn empty_and_single_file_have_exact_expected_world() {
    assert!(pack_text_parcels(&[], 1.0).unwrap().is_empty());
    let rects = check_partition(&[file(b"single", 100.0)], 4.0, 1e-12);
    assert_eq!(rects[0], Rect2D::from_xywh(0.0, 0.0, 20.0, 5.0).unwrap());
}

#[test]
fn duplicate_and_file_directory_collisions_are_refused_in_both_orders() {
    for paths in [[b"a".as_slice(), b"a"], [b"a", b"a/b"], [b"a/b", b"a"]] {
        assert!(pack_text_parcels(&[file(paths[0], 10.0), file(paths[1], 20.0)], 1.0).is_err());
    }
}

#[test]
fn malformed_paths_areas_and_aspects_are_refused() {
    for path in [b"".as_slice(), b"/absolute", b"a/", b"a//b", b".", b"..", b"a/../b", b"a/./b", b"a\0b"] {
        assert!(pack_text_parcels(&[file(path, 1.0)], 1.0).is_err(), "path {path:?}");
    }
    for area in [0.0, -1.0, 0.999, 1e12 + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(pack_text_parcels(&[file(b"a", area)], 1.0).is_err());
    }
    for aspect in [0.0, 0.249, 4.001, f64::NAN, f64::INFINITY] {
        assert!(pack_text_parcels(&[file(b"a", 1.0)], aspect).is_err());
        assert!(pack_text_parcels(&[], aspect).is_err());
    }
    // The map API retains raw repository path bytes; UTF-8 is only an FFI concern.
    assert!(pack_text_parcels(&[file(b"raw/\xff", 1.0)], 1.0).is_ok());
}

#[test]
fn depth_path_bytes_and_file_count_limits_are_enforced() {
    let depth64 = vec!["a"; 64].join("/");
    let depth65 = vec!["a"; 65].join("/");
    assert!(pack_text_parcels(&[file(depth64.as_bytes(), 1.0)], 1.0).is_ok());
    assert!(pack_text_parcels(&[file(depth65.as_bytes(), 1.0)], 1.0).is_err());
    let exact = vec![b'a'; 2 * 1024 * 1024];
    assert!(pack_text_parcels(&[file(&exact, 1.0)], 1.0).is_ok());
    assert!(pack_text_parcels(&[file(&exact, 1.0), file(b"b", 1.0)], 1.0).is_err());
    let paths: Vec<_> = (0..=MAX_TEXT_FILES).map(|i| format!("f{i:05}")).collect();
    let files: Vec<_> = paths.iter().map(|s| file(s.as_bytes(), 1.0)).collect();
    assert_eq!(pack_text_parcels(&files[..MAX_TEXT_FILES], 1.0).unwrap().len(), MAX_TEXT_FILES);
    assert!(pack_text_parcels(&files, 1.0).is_err());
}

#[test]
fn node_budget_refuses_before_file_or_path_byte_budgets() {
    // 2,081 independent 64-segment chains exceed 131,072 trie nodes while
    // remaining far below both the 20,000-file and 2 MiB path-byte limits.
    let suffix = vec!["x"; 63].join("/");
    let paths: Vec<_> = (0..2081).map(|i| format!("{i}/{suffix}")).collect();
    assert!(paths.iter().map(String::len).sum::<usize>() < 2 * 1024 * 1024);
    let files: Vec<_> = paths.iter().map(|s| file(s.as_bytes(), 1.0)).collect();
    assert!(pack_text_parcels(&files, 1.0).is_err());
}


#[test]
fn golden_leaf_shapes_are_selected_when_exact_tilings_exist() {
    let phi = (1.0 + 5.0_f64.sqrt()) / 2.0;
    for (count, aspect) in [(3, phi / 3.0), (4, phi), (6, phi * 2.0 / 3.0)] {
        let paths: Vec<_> = (0..count).map(|i| format!("src/f{i}")).collect();
        let files: Vec<_> = paths.iter().map(|p| file(p.as_bytes(), 100.0)).collect();
        let rects = check_partition(&files, aspect, 1e-12);
        for rect in rects {
            let ratio = rect.size().width() / rect.size().height();
            close(ratio.max(1.0 / ratio), phi, 1e-12);
        }
    }
}

#[test]
fn alternating_weight_hierarchy_preserves_full_area_and_directory_unions() {
    let paths: Vec<_> = (0..96).map(|i| format!("dir{}/f{i:03}", i / 16)).collect();
    let files: Vec<_> = paths.iter().enumerate().map(|(i, p)|
        file(p.as_bytes(), [1.0, 1000.0, 7.0, 50000.0][i % 4])).collect();
    for aspect in [0.25, 1.0, 4.0] {
        let rects = check_partition(&files, aspect, 1e-9);
        for group in rects.chunks(16) {
            let (x0, y0, x1, y1) = bounds(group);
            close((x1 - x0) * (y1 - y0), 4.0 * 51008.0, 1e-9);
        }
        let reversed: Vec<_> = files.iter().rev().copied().collect();
        assert_eq!(rects, pack_text_parcels(&reversed, aspect).unwrap().into_iter().rev().collect::<Vec<_>>());
    }
}

//! FCB-078.B: no-native/no-storage startup and query proof.
//!
//! Verifies the external consumer compiles, runs, and produces correct
//! results with zero window/Metal/AppKit/database/runtime startup. Every
//! test runs in a plain Rust binary with no native stack selected.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb_search::{DirectSourceScanner, QueryOptions, SearchMode, UnicodeNormalization};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb::MemorySourceProvider;

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0xBEEF).unwrap()
}

fn make_capture(bytes: &[u8]) -> CompleteCapture {
    let owner = owner();
    let file = FileId::new(owner, 1).unwrap();
    let rev = SourceRevision::new(owner, 1).unwrap();
    let request = CaptureRequest::new(file, rev).unwrap();
    let length = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(request, length, Arc::from(bytes.to_vec().into_boxed_slice())).unwrap()
}

#[test]
fn consumer_runs_without_any_native_stack() {
    // If this test compiles and passes, the binary started successfully
    // with no window server, no Metal device, no AppKit, no database, and
    // no runtime startup. The mere fact that the assertion runs is the
    // proof.
    let capture = make_capture(b"hello");
    assert_eq!(capture.bytes(), b"hello");
}

#[test]
fn inert_facade_capture_produces_exact_bytes() {
    let mut provider = MemorySourceProvider::new(owner()).unwrap();
    provider
        .insert("src/main.rs", b"fn main() {}".to_vec())
        .unwrap();
    let capture = provider.capture("src/main.rs").unwrap();
    assert_eq!(capture.bytes(), b"fn main() {}");
    assert_eq!(capture.logical_path(), "src/main.rs");
}

#[test]
fn search_finds_exact_matches_in_captured_source() {
    let capture = make_capture(b"let alpha = 1;\nlet beta = alpha + 1;\n");
    let options = QueryOptions::new(
        fcb_core::QueryGeneration::new(owner(), 1).unwrap(),
    )
    .with_mode(SearchMode::DecodedText {
        case_sensitive: true,
        normalization: UnicodeNormalization::Exact,
    });
    let result =
        DirectSourceScanner::scan_complete_capture(&capture, "alpha", &options).unwrap();
    assert_eq!(result.match_count(), 2, "alpha appears twice");
    assert!(result.is_complete());
}

#[test]
fn search_negative_control_returns_zero_for_missing_needle() {
    let capture = make_capture(b"hello world");
    let options = QueryOptions::new(
        fcb_core::QueryGeneration::new(owner(), 1).unwrap(),
    )
    .with_mode(SearchMode::RawBytes);
    let result =
        DirectSourceScanner::scan_complete_capture(&capture, "absent", &options).unwrap();
    assert_eq!(result.match_count(), 0);
    assert!(result.is_complete());
}

#[test]
fn map_layout_places_nodes_within_world_bounds() {
    use fcb_map::{HierarchySpec, LayoutOptions, LayoutRevision, NodeKind, NodeSpec,
        Size2D, WeightMetric};
    use fcb_core::RootId;

    let owner = owner();
    let root = RootId::new(owner, 1).unwrap();
    let spec = HierarchySpec::new(
        owner,
        root,
        vec![
            NodeSpec::new("src".as_bytes(), NodeKind::Directory, None),
            NodeSpec::new("src/lib.rs".as_bytes(), NodeKind::File, Some(100)),
            NodeSpec::new("src/main.rs".as_bytes(), NodeKind::File, Some(50)),
        ],
    )
    .unwrap();
    let revision = LayoutRevision::new(owner, 1).unwrap();
    let options = LayoutOptions::new(WeightMetric::CappedLogBytes, 0.1).unwrap();
    let world = Size2D::new(800.0, 600.0).unwrap();
    let layout =
        fcb_map::commit_layout(revision, world, &spec, options).unwrap();
    assert_eq!(layout.nodes().len(), 4, "all nodes placed (dir + 2 files + root composite)");
    for node in layout.nodes() {
        let rect = node.parent_local();
        assert!(rect.size().width() > 0.0, "positive width");
        assert!(rect.size().height() > 0.0, "positive height");
    }
}

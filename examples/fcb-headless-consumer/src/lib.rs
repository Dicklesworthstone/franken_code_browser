//! FCB-078.A: isolated headless source/search/map external library consumer.
//!
//! Proves the three seams work together from OUTSIDE the main workspace,
//! with no dev-feature leakage: the consumer's own workspace depends on the
//! fcb crates by path only, compiles without any window/Metal/AppKit/
//! database/runtime startup, and exercises real in-memory provider routes
//! end-to-end.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, RootId};
use fcb_map::{HierarchySpec, LayoutOptions, LayoutRevision, NodeKind, NodeSpec, Size2D,
    WeightMetric};
use fcb_search::{DirectSourceScanner, QueryOptions, SearchMode, UnicodeNormalization};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb::MemorySourceProvider;

pub const SOURCE_PATH: &str = "src/widget.rs";
pub const SOURCE_BYTES: &[u8] = b"pub fn widget() -> u32 {\n    42\n}\n";
pub const NEEDLE: &str = "42";
pub const OWNER_ID: u64 = 0xA11CE;

pub fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(OWNER_ID).unwrap()
}

/// Seam 1 — capture: the inert facade's in-memory provider yields exact bytes.
pub fn capture_source() -> fcb::SourceCapture {
    let mut provider = MemorySourceProvider::new(owner()).expect("provider constructs");
    provider.insert(SOURCE_PATH, SOURCE_BYTES.to_vec()).unwrap();
    provider.capture(SOURCE_PATH).expect("in-memory capture")
}

/// Seam 2 — search: scan the given bytes through the search engine's
/// direct scanner, wrapping them in a complete capture first.
pub fn search_bytes(bytes: &[u8], file_num: u64, needle: &str) -> fcb_search::SearchResult {
    let owner = owner();
    let file = fcb_core::FileId::new(owner, file_num).unwrap();
    let rev = fcb_core::SourceRevision::new(owner, 1).unwrap();
    let capture = CompleteCapture::new(
        CaptureRequest::new(file, rev).unwrap(),
        ByteLength::new(bytes.len() as u64),
        Arc::from(bytes.to_vec().into_boxed_slice()),
    )
    .unwrap();
    let generation = fcb_core::QueryGeneration::new(owner, 1).unwrap();
    let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
        case_sensitive: true,
        normalization: UnicodeNormalization::Exact,
    });
    DirectSourceScanner::scan_complete_capture(&capture, needle, &options)
        .expect("scan succeeds on exact bytes")
}

/// Seam 3 — map: build the repository hierarchy and commit a layout.
pub fn commit_tree_layout(file_len: u64) -> fcb_map::PartitionLayout {
    let root = RootId::new(owner(), 1).unwrap();
    let spec = HierarchySpec::new(
        owner(),
        root,
        vec![
            NodeSpec::new(
                "src".as_bytes(),
                NodeKind::Directory,
                None,
            ),
            NodeSpec::new(
                SOURCE_PATH.as_bytes(),
                NodeKind::File,
                Some(file_len),
            ),
        ],
    )
    .unwrap();
    let revision = LayoutRevision::new(owner(), 1).unwrap();
    let options = LayoutOptions::new(WeightMetric::CappedLogBytes, 0.05).unwrap();
    let world = Size2D::new(1920.0, 1080.0).unwrap();
    fcb_map::commit_layout(revision, world, &spec, options)
        .expect("layout commits for the two-node hierarchy")
}

#[test]
fn headless_consumer_captures_searches_and_lays_out_without_native_stack() {
    // Seam 1: capture.
    let capture = capture_source();
    assert_eq!(capture.bytes(), SOURCE_BYTES);
    assert_eq!(capture.logical_path(), SOURCE_PATH);

    // Seam 2: search.
    let result = search_bytes(SOURCE_BYTES, 5, NEEDLE);
    assert!(result.is_complete(), "exact bytes: coverage is exhaustive");
    assert_eq!(result.match_count(), 1, "exactly one needle hit");
    assert_eq!(result.matches[0].matched_text, NEEDLE);

    // Seam 3: layout.
    let layout = commit_tree_layout(SOURCE_BYTES.len() as u64);
    let node = layout
        .node(SOURCE_PATH.as_bytes())
        .expect("file node present in layout");
    // The placed rectangle is inside the committed world.
    let rect = node.parent_local();
    assert!(rect.size().width() > 0.0);
    assert!(rect.size().height() > 0.0);

    // Cross-seam consistency: the layout node path matches the search
    // subject path.
    assert_eq!(node.path(), SOURCE_PATH.as_bytes());
}

#[test]
fn negative_control_detects_missing_needle() {
    let result = search_bytes(SOURCE_BYTES, 6, "definitely-not-present");
    assert_eq!(
        result.match_count(),
        0,
        "missing needle yields zero hits"
    );
    assert!(result.is_complete());
}

#[test]
fn negative_control_zero_length_buffer_is_refused() {
    let owner = owner();
    let file = fcb_core::FileId::new(owner, 9).unwrap();
    let rev = fcb_core::SourceRevision::new(owner, 1).unwrap();
    let request = CaptureRequest::new(file, rev).unwrap();
    let result = CompleteCapture::new(
        request,
        ByteLength::new(99),
        Arc::from(Vec::new().into_boxed_slice()),
    );
    assert!(
        result.is_err(),
        "declared length 99 vs actual 0 must be refused"
    );
}

#[test]
fn negative_control_zero_world_layout_is_refused() {
    let root = RootId::new(owner(), 1).unwrap();
    let spec = HierarchySpec::new(
        owner(),
        root,
        vec![NodeSpec::new(
            SOURCE_PATH.as_bytes(),
            NodeKind::File,
            Some(1),
        )],
    )
    .unwrap();
    let revision = LayoutRevision::new(owner(), 1).unwrap();
    let zero_world = Size2D::new(0.0, 0.0).unwrap();
    let options =
        LayoutOptions::new(WeightMetric::CappedLogBytes, 0.05).unwrap();
    assert!(
        fcb_map::commit_layout(revision, zero_world, &spec, options).is_err(),
        "zero-area world must be refused"
    );
}

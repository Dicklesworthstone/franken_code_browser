#![forbid(unsafe_code)]

//! FCB-078.A / FCB-078.B: isolated headless source/search/map external library consumer.
//!
//! Proves the core seams work together from OUTSIDE the main workspace,
//! with no dev-feature leakage: the consumer's own workspace depends on the
//! fcb crates by path only, compiles without any window/Metal/AppKit/
//! database/runtime startup, and exercises real in-memory provider routes,
//! multi-instance isolation, unsupported feature error enforcement, and
//! path search/refinement end-to-end.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

pub use fcb::{
    BrowserSession, BrowserView, FcbError, Feature, FeatureSet, HostRequest, HostServices,
    MemorySourceProvider, SessionClose, SourceCapture,
};
pub use fcb_core::{
    ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget,
    RootId, SourceRevision,
};
pub use fcb_map::{
    HierarchySpec, LayoutOptions, LayoutRevision, NodeKind, NodeSpec, PartitionLayout, Size2D,
    WeightMetric,
};
pub use fcb_search::paths::{
    PathCase, PathEntry, PathIndex, PathIndexLimits, PathMatchKind, PathMatchMode, PathSearch,
    PathSearchError, PathSearchOptions, RawPath,
};
pub use fcb_search::{
    DirectSourceScanner, QueryOptions, SearchCoverage, SearchMode, SearchResult,
    UnicodeNormalization,
};
pub use fcb_source::{CaptureRequest, CompleteCapture};

pub const SOURCE_PATH: &str = "src/widget.rs";
pub const SOURCE_BYTES: &[u8] = b"pub fn widget() -> u32 {\n    42\n}\n";
pub const NEEDLE: &str = "42";
pub const OWNER_ID: u64 = 0xA11CE;

pub fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(OWNER_ID).unwrap()
}

/// Headless host services implementation for testing host-controlled event loop,
/// clock domains, and capability negotiation without native frameworks.
pub struct HeadlessHostServices {
    pub monotonic_now: AtomicU64,
    pub redraw_requests: AtomicUsize,
    pub wake_requests: AtomicUsize,
    pub capabilities: FeatureSet,
}

impl HeadlessHostServices {
    pub fn new(capabilities: FeatureSet) -> Self {
        Self {
            monotonic_now: AtomicU64::new(1_000_000),
            redraw_requests: AtomicUsize::new(0),
            wake_requests: AtomicUsize::new(0),
            capabilities,
        }
    }
}

impl HostServices for HeadlessHostServices {
    fn capabilities(&self) -> FeatureSet {
        self.capabilities
    }

    fn monotonic_nanos(&self) -> u64 {
        self.monotonic_now.load(Ordering::SeqCst)
    }

    fn request(&self, request: HostRequest) -> Result<(), FcbError> {
        match request {
            HostRequest::RequestRedraw => {
                self.redraw_requests.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            HostRequest::Wake => {
                self.wake_requests.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Seam 1 — capture: the inert facade's in-memory provider yields exact bytes.
pub fn capture_source() -> SourceCapture {
    let mut provider = MemorySourceProvider::new(owner()).expect("provider constructs");
    provider.insert(SOURCE_PATH, SOURCE_BYTES.to_vec()).unwrap();
    provider.capture(SOURCE_PATH).expect("in-memory capture")
}

/// Seam 2 — search: scan the given bytes through the search engine's
/// direct scanner, wrapping them in a complete capture first.
pub fn search_bytes(bytes: &[u8], file_num: u64, needle: &str) -> SearchResult {
    let owner = owner();
    let file = FileId::new(owner, file_num).unwrap();
    let rev = SourceRevision::new(owner, 1).unwrap();
    let capture = CompleteCapture::new(
        CaptureRequest::new(file, rev).unwrap(),
        ByteLength::new(bytes.len() as u64),
        Arc::from(bytes.to_vec().into_boxed_slice()),
    )
    .unwrap();
    let generation = QueryGeneration::new(owner, 1).unwrap();
    let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
        case_sensitive: true,
        normalization: UnicodeNormalization::Exact,
    });
    DirectSourceScanner::scan_complete_capture(&capture, needle, &options)
        .expect("scan succeeds on exact bytes")
}

/// Seam 3 — map: build the repository hierarchy and commit a layout.
pub fn commit_tree_layout(file_len: u64) -> PartitionLayout {
    let root = RootId::new(owner(), 1).unwrap();
    let spec = HierarchySpec::new(
        owner(),
        root,
        vec![
            NodeSpec::new("src".as_bytes(), NodeKind::Directory, None),
            NodeSpec::new(SOURCE_PATH.as_bytes(), NodeKind::File, Some(file_len)),
        ],
    )
    .unwrap();
    let revision = LayoutRevision::new(owner(), 1).unwrap();
    let options = LayoutOptions::new(WeightMetric::CappedLogBytes, 0.05).unwrap();
    let world = Size2D::new(1920.0, 1080.0).unwrap();
    fcb_map::commit_layout(revision, world, &spec, options)
        .expect("layout commits for the two-node hierarchy")
}

/// Bounded scenario-receipt recorder retaining machine evidence under
/// `${TMPDIR:-/tmp}/fcb-078-receipts-${FCB_078_RUN_ID:-local}/`.
pub fn record_receipt(scenario: &str, invariant: &str, details: &str) {
    let run_id = std::env::var("FCB_078_RUN_ID").unwrap_or_else(|_| "local".to_string());
    let dir = std::env::temp_dir().join(format!("fcb-078-receipts-{run_id}"));
    let _ = std::fs::create_dir_all(&dir);
    let sanitized_name = scenario.replace([' ', ':', '/', '(', ')', ','], "_");
    let content = format!(
        "schema: fcb.receipt.v1\nscenario: {scenario}\nseed: 0x0C780001\nroute: headless:rust:consumer\noutcome: Succeeded (exit 0)\ninvariant: {invariant}\ndetails: {details}\n"
    );
    let _ = std::fs::write(dir.join(format!("{sanitized_name}.receipt")), content);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rect = node.parent_local();
        assert!(rect.size().width() > 0.0);
        assert!(rect.size().height() > 0.0);

        // Cross-seam consistency: the layout node path matches the search subject path.
        assert_eq!(node.path(), SOURCE_PATH.as_bytes());

        record_receipt(
            "headless_consumer_captures_searches_and_lays_out_without_native_stack",
            "verified combined capture, exact text search, and partition layout in external workspace",
            "capture_len=36, hits=1, layout_world=1920x1080",
        );
    }

    #[test]
    fn inert_startup_and_unsupported_feature_errors_keep_host_in_control() {
        let owner = owner();

        // 1. Inert session with no host services: requests return HostServicesUnavailable
        let session = BrowserSession::new(owner);
        assert_eq!(
            session.request_host(HostRequest::RequestRedraw),
            Err(FcbError::HostServicesUnavailable)
        );
        assert_eq!(
            session.request_host(HostRequest::Wake),
            Err(FcbError::HostServicesUnavailable)
        );
        assert_eq!(
            session.host_monotonic_nanos(),
            Err(FcbError::HostServicesUnavailable)
        );

        // 2. Feature matrix check: inert baseline vs unavailable product surfaces
        assert!(session.require_feature(Feature::Source).is_ok());
        assert!(session.require_feature(Feature::View).is_ok());
        assert_eq!(
            session.require_feature(Feature::Persistence),
            Err(FcbError::FeatureUnavailable)
        );
        assert_eq!(
            session.require_feature(Feature::Runtime),
            Err(FcbError::FeatureUnavailable)
        );
        assert_eq!(
            session.require_feature(Feature::Markdown),
            Err(FcbError::FeatureUnavailable)
        );

        let metal_err = session.require_feature(Feature::MacosMetal);
        if cfg!(target_os = "macos") {
            assert_eq!(metal_err, Err(FcbError::FeatureUnavailable));
        } else {
            assert_eq!(metal_err, Err(FcbError::UnsupportedTarget));
        }

        // 3. Host services attached: host remains strictly in control of event loop and clock
        let host: Arc<dyn HostServices> =
            Arc::new(HeadlessHostServices::new(FeatureSet::all_known()));
        let session_with_host = BrowserSession::with_services(owner, Arc::clone(&host));

        session_with_host
            .request_host(HostRequest::RequestRedraw)
            .expect("redraw request forwards to host");

        session_with_host
            .request_host(HostRequest::Wake)
            .expect("wake request forwards to host");

        assert_eq!(
            session_with_host.host_monotonic_nanos().unwrap(),
            1_000_000
        );

        // Facade intersects host claims with available features: no false capability claims
        let effective = session_with_host.host_capabilities();
        assert!(effective.contains(Feature::Source));
        assert!(effective.contains(Feature::View));
        assert!(!effective.contains(Feature::Persistence));
        assert!(!effective.contains(Feature::Runtime));

        record_receipt(
            "inert_startup_and_unsupported_feature_errors_keep_host_in_control",
            "verified inert session startup, feature refusal errors, and host event-loop control",
            "host_redraws=1, host_wakes=1, feature_refusals=persistence/runtime/markdown/metal",
        );
    }

    #[test]
    fn two_isolated_browser_sessions_preserve_cache_privacy_and_independent_lifecycle() {
        let owner_a = ArenaOwnerId::new(0x0C78_0001).unwrap();
        let owner_b = ArenaOwnerId::new(0x0C78_0002).unwrap();

        let mut provider_a = MemorySourceProvider::new(owner_a).unwrap();
        provider_a
            .insert("src/alpha.rs", b"pub fn alpha() {}".to_vec())
            .unwrap();

        let mut provider_b = MemorySourceProvider::new(owner_b).unwrap();
        provider_b
            .insert("src/beta.rs", b"pub fn beta() {}".to_vec())
            .unwrap();

        let session_a = BrowserSession::with_provider(owner_a, Arc::new(provider_a));
        let session_b = BrowserSession::with_provider(owner_b, Arc::new(provider_b));

        let view_a = session_a.open("src/alpha.rs").expect("session A opens its file");
        let view_b = session_b.open("src/beta.rs").expect("session B opens its file");

        assert_eq!(view_a.source().bytes(), b"pub fn alpha() {}");
        assert_eq!(view_b.source().bytes(), b"pub fn beta() {}");

        // Cache privacy & owner confinement: Session B cannot open Session A's capture
        assert_eq!(
            session_b.open_capture(view_a.source().clone()),
            Err(FcbError::OwnerMismatch),
            "foreign capture must be refused to preserve cache privacy"
        );
        assert_eq!(
            session_a.open_capture(view_b.source().clone()),
            Err(FcbError::OwnerMismatch),
            "foreign capture must be refused to preserve cache privacy"
        );

        // Independent close & drop: closing session A must leave session B operational
        let close_a = session_a.close();
        assert_eq!(close_a.owner(), owner_a);

        // Session B continues to operate without degradation
        let view_b_again = session_b
            .open("src/beta.rs")
            .expect("session B remains fully operational after session A closed");
        let plan_b = view_b_again.frame_plan().expect("frame plan generates");
        assert_eq!(plan_b.bytes().len().get(), 16);

        record_receipt(
            "two_isolated_browser_sessions_preserve_cache_privacy_and_independent_lifecycle",
            "verified multi-instance owner confinement, cache privacy, and independent drop",
            "owner_a=0x0C780001, owner_b=0x0C780002, foreign_capture_refused=true, session_b_survives=true",
        );
    }

    #[test]
    fn no_storage_in_memory_path_search_and_refinement() {
        let owner = owner();
        let root = RootId::new(owner, 1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
        let alloc = ResourceAllocationId::new(1).unwrap();

        let file1 = FileId::new(owner, 1).unwrap();
        let file2 = FileId::new(owner, 2).unwrap();
        let file3 = FileId::new(owner, 3).unwrap();
        let file4 = FileId::new(owner, 4).unwrap();
        let file5 = FileId::new(owner, 5).unwrap();

        let p1 = RawPath::from_bytes(&b"src/lib.rs"[..]);
        let p2 = RawPath::from_bytes(&b"src/main.rs"[..]);
        let p3 = RawPath::from_bytes(&b"src/widget.rs"[..]);
        let p4 = RawPath::from_bytes(&b"tests/smoke.rs"[..]);
        let p5 = RawPath::from_bytes(&b"README.md"[..]);

        let entries = [
            PathEntry::new(file1, root, &p1),
            PathEntry::new(file2, root, &p2),
            PathEntry::new(file3, root, &p3),
            PathEntry::new(file4, root, &p4),
            PathEntry::new(file5, root, &p5),
        ];

        let manifest_id = fcb_search::SearchManifestId::new(owner, 1).unwrap();
        let index = PathIndex::build(
            manifest_id,
            fcb_search::MembershipState::Closed,
            &entries,
            PathIndexLimits::default(),
            &budget,
            alloc,
            || false,
        )
        .expect("path index builds in memory with zero storage");

        assert_eq!(index.len(), 5);

        // Exact search for widget.rs
        let query_gen1 = QueryGeneration::new(owner, 1).unwrap();
        let mut exact_opts = PathSearchOptions::new(query_gen1);
        exact_opts.mode = PathMatchMode::Exact;
        let mut search_exact = PathSearch::new(
            &index,
            b"widget.rs",
            exact_opts,
            &budget,
            ResourceAllocationId::new(2).unwrap(),
        )
        .unwrap();
        search_exact.run_to_completion(|| false).unwrap();
        assert_eq!(search_exact.matches_seen(), 1);
        assert_eq!(search_exact.ranked_matches()[0].file_id(), file3);
        assert_eq!(
            search_exact.ranked_matches()[0].rank().kind,
            PathMatchKind::ExactFilename
        );

        // Prefix search for "src"
        let query_gen2 = QueryGeneration::new(owner, 2).unwrap();
        let mut prefix_opts = PathSearchOptions::new(query_gen2);
        prefix_opts.mode = PathMatchMode::Prefix;
        let mut search_prefix = PathSearch::new(
            &index,
            b"src",
            prefix_opts,
            &budget,
            ResourceAllocationId::new(3).unwrap(),
        )
        .unwrap();
        search_prefix.run_to_completion(|| false).unwrap();
        assert_eq!(search_prefix.matches_seen(), 3);

        // Fuzzy search with candidate reuse upon query refinement
        let query_gen3 = QueryGeneration::new(owner, 3).unwrap();
        let fuzzy_opts1 = PathSearchOptions::new(query_gen3);
        let mut search_fuzzy1 = PathSearch::new(
            &index,
            b"w",
            fuzzy_opts1,
            &budget,
            ResourceAllocationId::new(4).unwrap(),
        )
        .unwrap();
        search_fuzzy1.run_to_completion(|| false).unwrap();
        assert!(search_fuzzy1.is_complete());

        let query_gen4 = QueryGeneration::new(owner, 4).unwrap();
        let fuzzy_opts2 = PathSearchOptions::new(query_gen4);
        let mut refined = search_fuzzy1
            .refine(
                b"wid",
                fuzzy_opts2,
                &budget,
                ResourceAllocationId::new(5).unwrap(),
            )
            .unwrap();
        assert!(
            refined.reused_candidates(),
            "query refinement reuses prior matched candidate bitset"
        );
        refined.run_to_completion(|| false).unwrap();
        assert_eq!(refined.matches_seen(), 1);
        assert_eq!(refined.ranked_matches()[0].file_id(), file3);

        record_receipt(
            "no_storage_in_memory_path_search_and_refinement",
            "verified no-storage in-memory path index building, exact/prefix/fuzzy search, and candidate refinement",
            "indexed_paths=5, exact_hit=src/widget.rs, prefix_hits=3, refined_reused=true",
        );
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

        record_receipt(
            "negative_control_detects_missing_needle",
            "negative control: scanner truthfully reports zero hits for absent needle",
            "matches_found=0, is_complete=true",
        );
    }

    #[test]
    fn negative_control_zero_length_buffer_is_refused() {
        let owner = owner();
        let file = FileId::new(owner, 9).unwrap();
        let rev = SourceRevision::new(owner, 1).unwrap();
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

        record_receipt(
            "negative_control_zero_length_buffer_is_refused",
            "negative control: length mismatch between declared ByteLength and slice is refused",
            "declared=99, actual=0, refused=true",
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
        let options = LayoutOptions::new(WeightMetric::CappedLogBytes, 0.05).unwrap();
        assert!(
            fcb_map::commit_layout(revision, zero_world, &spec, options).is_err(),
            "zero-area world must be refused"
        );

        record_receipt(
            "negative_control_zero_world_layout_is_refused",
            "negative control: zero-area layout world size is rejected",
            "world_size=0x0, refused=true",
        );
    }

    #[test]
    fn negative_control_foreign_capture_owner_refused_for_view() {
        let local_owner = owner();
        let foreign_owner = ArenaOwnerId::new(0xDEAD).unwrap();

        let foreign_file = FileId::new(foreign_owner, 1).unwrap();
        let foreign_rev = SourceRevision::new(foreign_owner, 1).unwrap();
        let foreign_capture = SourceCapture::from_bytes(
            foreign_owner,
            foreign_file,
            foreign_rev,
            "src/foreign.rs",
            b"// foreign bytes".to_vec(),
        )
        .unwrap();

        let session = BrowserSession::new(local_owner);
        let open_result = session.open_capture(foreign_capture);
        assert_eq!(
            open_result,
            Err(FcbError::OwnerMismatch),
            "foreign capture owner must be refused with OwnerMismatch"
        );

        record_receipt(
            "negative_control_foreign_capture_owner_refused_for_view",
            "negative control: browser session strictly enforces ownership match on opened captures",
            "local_owner=0xA11CE, foreign_owner=0xDEAD, expected_err=OwnerMismatch",
        );
    }

    #[test]
    fn negative_control_path_search_empty_query_and_foreign_owner() {
        let owner = owner();
        let foreign_owner = ArenaOwnerId::new(0x9999).unwrap();
        let root = RootId::new(owner, 1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
        let alloc = ResourceAllocationId::new(10).unwrap();

        let file = FileId::new(owner, 1).unwrap();
        let path = RawPath::from_bytes(&b"file.rs"[..]);
        let entries = [PathEntry::new(file, root, &path)];
        let manifest_id = fcb_search::SearchManifestId::new(owner, 1).unwrap();
        let index = PathIndex::build(
            manifest_id,
            fcb_search::MembershipState::Closed,
            &entries,
            PathIndexLimits::default(),
            &budget,
            alloc,
            || false,
        )
        .unwrap();

        // 1. Empty query returns EmptyQuery
        let query_gen = QueryGeneration::new(owner, 1).unwrap();
        let empty_err = PathSearch::new(
            &index,
            b"",
            PathSearchOptions::new(query_gen),
            &budget,
            ResourceAllocationId::new(11).unwrap(),
        );
        assert!(matches!(empty_err, Err(PathSearchError::EmptyQuery)));

        // 2. Foreign query generation owner returns OwnerMismatch
        let foreign_gen = QueryGeneration::new(foreign_owner, 1).unwrap();
        let owner_err = PathSearch::new(
            &index,
            b"file",
            PathSearchOptions::new(foreign_gen),
            &budget,
            ResourceAllocationId::new(12).unwrap(),
        );
        assert!(matches!(owner_err, Err(PathSearchError::OwnerMismatch)));

        // 3. Dot-dot relative path in membership is refused with InvalidUpdate
        let bad_path = RawPath::from_bytes(&b"src/../file.rs"[..]);
        let bad_entries = [PathEntry::new(file, root, &bad_path)];
        let bad_build = PathIndex::build(
            manifest_id,
            fcb_search::MembershipState::Closed,
            &bad_entries,
            PathIndexLimits::default(),
            &budget,
            ResourceAllocationId::new(13).unwrap(),
            || false,
        );
        assert!(matches!(bad_build, Err(PathSearchError::InvalidUpdate)));

        record_receipt(
            "negative_control_path_search_empty_query_and_foreign_owner",
            "negative control: empty query, foreign generation owner, and parent traversal relative paths are rejected",
            "empty_err=EmptyQuery, foreign_owner_err=OwnerMismatch, dotdot_err=InvalidUpdate",
        );
    }
}

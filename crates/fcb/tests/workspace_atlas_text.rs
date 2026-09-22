#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", unix))]

//! Real discovery/capture -> indexed text search -> exact atlas selection.
//! These are portable public-consumer tests, not native presentation evidence.
use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D, SourceRevision};
use fcb::map::{AtlasNodeId, LayoutOptions, LayoutRevision, QueryGeneration,
    ResourceAllocationId, ResourceBudget, RootId};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
use fcb::map::workspace::text_search::{AtlasTextCounts, AtlasTextLimits,
    WorkspaceTextError, WorkspaceTextSource};
use fcb::search::{CompleteCapture, IndexLimits, RawPath, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCaptures, WorkspaceCatalog,
    WorkspaceError, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, SourceError};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(711).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-atlas-text-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn catalog(root: &Path, limits: WorkspaceLimits, budget: &ResourceBudget) -> WorkspaceCatalog {
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(root));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 100).unwrap(), limits, false, budget, allocation(1)).unwrap();
    for _ in 0..4097 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready); catalog
}
fn captures<'a>(catalog: &'a WorkspaceCatalog, root: &Path, budget: &ResourceBudget) -> WorkspaceCaptures<'a> {
    let mut captures = WorkspaceCaptures::new(catalog, SourceRevision::new(owner(), 500).unwrap(), budget, allocation(2)).unwrap();
    for _ in 0..=catalog.entries().len() {
        if captures.finished() { break; }
        captures.step(&CancelFlag::new(), |request, path, allowance| {
            let bytes = fs::read(root.join(path.raw().to_path_buf())).unwrap();
            assert!(bytes.len() <= allowance);
            CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
        }).unwrap();
    }
    assert!(captures.finished()); captures
}
fn atlas<'a>(catalog: &'a WorkspaceCatalog, budget: &ResourceBudget) -> WorkspaceAtlas<'a> {
    WorkspaceAtlas::build(catalog, &fcb::map::workspace::AtlasScope::All, LayoutRevision::new(owner(), 1).unwrap(),
        Size2D::new(1024.0, 768.0).unwrap(), LayoutOptions::modest(),
        WorkspaceAtlasLimits::default(), budget, allocation(20), || false).unwrap()
}
fn node(atlas: &WorkspaceAtlas<'_>, path: &[u8]) -> AtlasNodeId {
    let ordinal = atlas.layout().nodes().iter().position(|n| n.path() == path).unwrap();
    AtlasNodeId::new(atlas.layout().root(), atlas.layout().revision(), ordinal as u32)
}
fn allocs(n: u64) -> [ResourceAllocationId; 3] { [allocation(n), allocation(n + 1), allocation(n + 2)] }

#[test]
fn source_text_occurrences_and_distinct_file_counts_preserve_atlas_geometry() {
    let root = root(); fs::create_dir(root.join("src")).unwrap(); fs::create_dir(root.join("src-old")).unwrap();
    fs::write(root.join("src/a.rs"), b"needle needle").unwrap();
    fs::write(root.join("src/b.rs"), b"needle").unwrap();
    fs::write(root.join("src-old/a.rs"), b"needle").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let geometry = atlas.layout().clone();
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(overlay.is_complete()); assert_eq!(overlay.hits().len(), 4);
    assert_eq!(overlay.retained_matches_in(node(&atlas, b"src")).unwrap(), AtlasTextCounts { occurrences: 3, files: 2 });
    assert_eq!(overlay.retained_matches_in(node(&atlas, b"src/a.rs")).unwrap(), AtlasTextCounts { occurrences: 2, files: 1 });
    assert_eq!(overlay.retained_matches_in(node(&atlas, b"")).unwrap(), AtlasTextCounts { occurrences: 4, files: 3 });
    let other = index.search("absent", generation(2), AtlasTextLimits::default(), &budget, allocs(8), || false).unwrap();
    assert!(other.is_complete()); assert!(other.hits().is_empty());
    assert_eq!(atlas.layout(), &geometry, "query changes must never repack geography");
    for (i, hit) in overlay.hits().iter().enumerate() {
        assert_eq!(atlas.file(hit.node()).unwrap(), hit.file());
        assert_eq!(overlay.select_hit(&source, i, generation(1)).unwrap().original_bytes(), b"needle");
    }
}

#[test]
fn utf16_hits_keep_original_bytes_after_live_replacement_and_index_drop() {
    let root = root();
    let mut original = vec![0xff, 0xfe];
    for unit in "head\r\nneedle 😀 needle\r\n".encode_utf16() { original.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("wide.txt"), &original).unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(overlay.is_complete()); assert_eq!(overlay.hits().len(), 2);
    assert!(overlay.report().fallback_attempts() > 0, "UTF-8 prefilter cannot reject a UTF-16 text query");
    drop(index);
    fs::write(root.join("wide.txt"), b"completely different current source").unwrap();
    let selection = overlay.select_hit(&source, 0, generation(1)).unwrap();
    assert_eq!(selection.matched_text(), "needle");
    assert_eq!(selection.hit().original_range().start().get(), 14);
    assert_eq!(selection.original_bytes(), b"n\0e\0e\0d\0l\0e\0");
    assert_eq!(selection.capture().bytes(), original);
    assert_eq!(selection.capture().bytes().as_ptr(), captures.capture(selection.hit().file()).unwrap().bytes().as_ptr());
}

#[test]
fn overlapping_occurrences_and_query_looking_literals_use_shared_exact_semantics() {
    let root = root(); fs::write(root.join("a.rs"), b"aaaa needle -path:src").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlap = index.search("aaa", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(overlap.is_complete()); assert_eq!(overlap.hits().len(), 2);
    assert_eq!(overlap.hits()[1].original_range().start().get(), 1);
    let literal = index.search("needle -path:src", generation(2), AtlasTextLimits::default(), &budget, allocs(8), || false).unwrap();
    assert_eq!(literal.hits().len(), 1);
    assert_eq!(literal.select_hit(&source, 0, generation(2)).unwrap().original_bytes(), b"needle -path:src");
}

#[test]
fn capture_refusals_never_become_complete_negative_searches() {
    let root = root(); fs::write(root.join("small.rs"), b"needle").unwrap();
    fs::write(root.join("large.rs"), b"needle in an unavailable larger file").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits { max_file_bytes: 8, ..WorkspaceLimits::default() }, &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(!overlay.is_complete()); assert_eq!(overlay.hits().len(), 1);
    assert_eq!(overlay.report().unavailable_files().len(), 1);
    assert_eq!(overlay.retained_matches_in(node(&atlas, b"")).unwrap(), AtlasTextCounts { occurrences: 1, files: 1 });
    assert!(captures.file_failure(overlay.report().unavailable_files()[0]).is_some());
}

#[test]
fn discovery_query_byte_and_hit_limits_are_independently_partial() {
    let root = root(); fs::write(root.join("a.rs"), b"needle needle").unwrap(); fs::write(root.join("b.rs"), b"needle").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits { max_files: 1, ..WorkspaceLimits::default() }, &budget);
    assert!(!catalog.discovery_complete());
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let missing = index.search("absent", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(missing.hits().is_empty()); assert!(!missing.is_complete());
    let limited = index.search("needle", generation(2), AtlasTextLimits { max_matches: 1, ..AtlasTextLimits::default() }, &budget, allocs(8), || false).unwrap();
    assert_eq!(limited.hits().len(), 1); assert!(!limited.is_complete());
    let bytes = index.search("needle", generation(3), AtlasTextLimits { max_bytes_scanned: 2, ..AtlasTextLimits::default() }, &budget, allocs(11), || false).unwrap();
    assert!(!bytes.is_complete()); assert!(bytes.hits().is_empty());
    let zero = index.search("needle", generation(4), AtlasTextLimits { max_matches: 0, ..AtlasTextLimits::default() }, &budget, allocs(14), || false).unwrap();
    assert!(zero.hits().is_empty()); assert!(!zero.is_complete());
    assert!(zero.select_hit(&source, 0, generation(4)).is_err());
}

#[test]
fn source_binding_rejects_equal_ids_from_other_catalogs_and_capture_sets() {
    let root = root(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let other_budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    let other_catalog = self::catalog(&root, WorkspaceLimits::default(), &other_budget);
    let other_captures = self::captures(&other_catalog, &root, &other_budget);
    assert!(matches!(WorkspaceTextSource::new(&atlas, &other_captures, &budget, allocation(3)), Err(WorkspaceTextError::WrongSource)));
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let replacement_source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(8)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(matches!(overlay.validate_delivery(&replacement_source, generation(1)), Err(WorkspaceTextError::WrongSource)));
    assert!(overlay.validate_delivery(&source, generation(2)).is_err());
    assert!(overlay.select_hit(&replacement_source, 0, generation(1)).is_err());
}

#[test]
fn canceled_replacement_preserves_old_results_and_releases_new_admission() {
    let root = root(); fs::write(root.join("a.rs"), b"needle needle").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let captures = captures(&catalog, &root, &budget); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let old = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    let mut checks = 0;
    assert!(index.search("needle", generation(2), AtlasTextLimits::default(), &budget, allocs(8), || { checks += 1; checks > 4 }).is_err());
    let replacement = index.search("needle", generation(3), AtlasTextLimits::default(), &budget, allocs(8), || false).unwrap();
    assert!(replacement.is_complete()); assert_eq!(old.hits().len(), 2);
    assert_eq!(old.select_hit(&source, 0, generation(1)).unwrap().original_bytes(), b"needle");
    catalog.grant().revoke();
    assert!(old.validate_delivery(&source, generation(1)).is_err());
    assert!(old.select_hit(&source, 0, generation(1)).is_err());
    assert!(old.retained_matches_in(node(&atlas, b"")).is_err());
}

#[test]
fn unfinished_captures_cannot_publish_and_index_refusals_still_scan_exactly() {
    let root = root(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(), &budget, allocation(2)).unwrap();
    let atlas = atlas(&catalog, &budget);
    assert!(matches!(WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)), Err(WorkspaceTextError::Workspace(WorkspaceError::Pending))));
    captures.step(&CancelFlag::new(), |request, _, _| CompleteCapture::new(request, ByteLength::new(6), Arc::from(b"needle".as_slice()))).unwrap();
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits { max_source_bytes_per_file: 0, ..IndexLimits::default() }, &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(overlay.is_complete()); assert_eq!(overlay.hits().len(), 1); assert_eq!(overlay.report().fallback_attempts(), 1);
}

#[test]
fn empty_catalog_is_a_complete_empty_scope_without_a_capture_callback() {
    let root = root(); let budget = budget(); let catalog = catalog(&root, WorkspaceLimits::default(), &budget);
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner(), 1).unwrap(), &budget, allocation(2)).unwrap();
    captures.step(&CancelFlag::new(), |_, _, _| -> Result<CompleteCapture, SourceError> { panic!("empty scope must not read source") }).unwrap();
    let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget, allocs(5), || false).unwrap();
    assert!(overlay.is_complete()); assert!(overlay.hits().is_empty());
    assert_eq!(overlay.retained_matches_in(node(&atlas, b"")).unwrap(), AtlasTextCounts::default());
    assert!(index.search("", generation(2), AtlasTextLimits::default(), &budget, allocs(8), || false).is_err());
    assert!(index.search("needle", generation(2), AtlasTextLimits { max_matches: 4097, ..AtlasTextLimits::default() }, &budget, allocs(8), || false).is_err());
}

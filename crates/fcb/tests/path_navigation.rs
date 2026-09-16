#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Only the public fcb facade participates: metadata -> progressive path query
//! -> selected identity -> explicit host capture -> source view. No ambient I/O.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, FcbError, FileId, SourceCapture, SourceProvider, SourceRevision};
use fcb::search::{MembershipState, NativeSourceIdentity, PathEntry, PathIndex, PathIndexLimits,
    PathNavigationTarget, PathSearch, PathSearchOptions, PathStepBudget, QueryGeneration,
    RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(351).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn root(id: u64) -> RootId { RootId::new(owner(), id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn manifest(id: u64) -> SearchManifestId { SearchManifestId::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(8 * 1024 * 1024)).unwrap() }
fn source(id: u64, revision: u64, label: &str) -> SourceCapture {
    SourceCapture::from_bytes(owner(), file(id), SourceRevision::new(owner(), revision).unwrap(),
        label, b"fn actual_source() {}\n".to_vec()).unwrap()
}
fn index(paths: &[RawPath], budget: &ResourceBudget) -> PathIndex {
    let entries: Vec<_> = paths.iter().enumerate().map(|(i, path)| PathEntry::new(file(i as u64 + 1), root(1), path)).collect();
    PathIndex::build(manifest(1), MembershipState::Closed, &entries,
        PathIndexLimits::default(), budget, allocation(1), || false).unwrap()
}

struct Probe { calls: AtomicUsize }
impl SourceProvider for Probe {
    fn capture(&self, path: &str) -> Result<SourceCapture, FcbError> {
        assert_eq!(path, "src/Foo.rs");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(source(1, 9, path))
    }
}

#[test]
fn path_search_reads_no_source_and_open_does_not_repeat_the_host_capture() {
    let budget = budget();
    let paths = [RawPath::from_str("src/Foo.rs"), RawPath::from_str("src/foo.rs")];
    let index = index(&paths, &budget);
    let provider = Arc::new(Probe { calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner(), provider.clone());
    let mut query = PathSearch::new(&index, b"Foo.rs", PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(query.matches_seen(), 2);
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    assert_eq!(target.native_path().as_bytes(), b"src/Foo.rs");
    drop(query); // The selected target borrows the index, not a mutable row list.
    let capture = provider.capture("src/Foo.rs").unwrap();
    let bytes = capture.bytes().as_ptr();
    let view = session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(1), path: &paths[0] }, capture).unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(view.source().bytes().as_ptr(), bytes);
    assert_eq!(view.source().revision().get(), 9);
    assert_eq!(view.frame_plan().unwrap().file(), file(1));
    assert_eq!(view.frame_plan().unwrap().source(), view.source().revision());
}

#[test]
fn source_navigation_rejects_stale_requests_wrong_roots_aliases_and_files() {
    let budget = budget();
    let paths = [RawPath::from_str("src/Foo.rs")];
    let index = index(&paths, &budget);
    let session = BrowserSession::new(owner());
    let mut query = PathSearch::new(&index, b"Foo.rs", PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    let native = NativeSourceIdentity { root: root(1), path: &paths[0] };
    assert_eq!(session.open_path_target(&target, manifest(1), generation(2), native, source(1, 1, "label")), Err(FcbError::StaleGeneration));
    assert_eq!(session.open_path_target(&target, manifest(2), generation(1), native, source(1, 1, "label")), Err(FcbError::StaleGeneration));
    assert_eq!(session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(2), ..native }, source(1, 1, "label")), Err(FcbError::OwnerMismatch));
    let alias = RawPath::from_str("src/foo.rs");
    assert_eq!(session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(1), path: &alias }, source(1, 1, "label")), Err(FcbError::StaleGeneration));
    assert_eq!(session.open_path_target(&target, manifest(1), generation(1), native, source(2, 1, "label")), Err(FcbError::SourceNotFound));
    let foreign = BrowserSession::new(ArenaOwnerId::new(352).unwrap());
    assert_eq!(foreign.open_path_target(&target, manifest(1), generation(1), native, source(1, 1, "label")), Err(FcbError::OwnerMismatch));
    assert!(PathNavigationTarget::from_search(&query, file(99)).is_err());
}

#[test]
fn raw_non_utf8_navigation_never_uses_escaped_display_labels_as_identity() {
    let budget = budget();
    let paths = [RawPath::from_bytes(b"src/\xff.rs".as_slice())];
    let index = index(&paths, &budget);
    let session = BrowserSession::new(owner());
    let mut query = PathSearch::new(&index, &[0xff], PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    let label = paths[0].display_escaped().to_string();
    let wrong_native = RawPath::from_str(&label);
    assert_ne!(wrong_native.as_bytes(), paths[0].as_bytes());
    assert_eq!(session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(1), path: &wrong_native }, source(1, 1, &label)), Err(FcbError::StaleGeneration));
    let view = session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(1), path: &paths[0] }, source(1, 1, &label)).unwrap();
    assert_eq!(view.source().bytes(), b"fn actual_source() {}\n");
    assert_eq!(view.source().logical_path(), label);
}

#[test]
fn a_pinned_result_can_be_opened_after_it_falls_out_of_the_top_k() {
    let budget = budget();
    let paths = [RawPath::from_str("f_a_o_o.rs"), RawPath::from_str("foo")];
    let index = index(&paths, &budget);
    let session = BrowserSession::new(owner());
    let mut options = PathSearchOptions::new(generation(1)); options.max_results = 1;
    let mut query = PathSearch::new(&index, b"foo", options, &budget, allocation(2)).unwrap();
    query.step(PathStepBudget { max_candidates: 1, ..Default::default() }, generation(1), || false).unwrap();
    query.select(file(1)).unwrap();
    query.run_to_completion(|| false).unwrap();
    query.release_ordering();
    assert_eq!(query.visible_matches()[0].file_id(), file(2));
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    let view = session.open_path_target(&target, manifest(1), generation(1),
        NativeSourceIdentity { root: root(1), path: &paths[0] }, source(1, 4, "f_a_o_o.rs")).unwrap();
    assert_eq!(view.source().file(), file(1));
}

#[test]
fn renames_invalidate_old_navigation_but_do_not_destroy_old_captures() {
    let budget = budget();
    let paths = [RawPath::from_str("old.rs")];
    let old = index(&paths, &budget);
    let session = BrowserSession::new(owner());
    let mut query = PathSearch::new(&old, b"old.rs", PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    let capture = source(1, 1, "old.rs");
    let retained = session.open_path_target(&target, old.id(), generation(1),
        NativeSourceIdentity { root: root(1), path: &paths[0] }, capture.clone()).unwrap();
    let renamed = RawPath::from_str("new.rs");
    let next = old.updated(manifest(2), MembershipState::Closed,
        &[PathEntry::new(file(1), root(1), &renamed)], &[], &budget, allocation(3), || false).unwrap();
    assert_eq!(session.open_path_target(&target, next.id(), generation(1),
        NativeSourceIdentity { root: root(1), path: &paths[0] }, capture), Err(FcbError::StaleGeneration));
    assert_eq!(retained.source().bytes(), b"fn actual_source() {}\n");
    assert_eq!(retained.source().logical_path(), "old.rs");
}

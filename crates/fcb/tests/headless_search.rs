#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Uses only public `fcb` imports: provider -> retained capture -> index -> hit
//! -> reading view. Selected features require no Apple framework or database.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FcbError,
    Feature, FileId, SourceCapture, SourceProvider, SourceRevision};
use fcb::search::{EphemeralIndex, IndexLimits, ManifestLimits, MembershipState,
    ParsedQuery, QueryGeneration, QueryOptions, ResourceAllocationId, ResourceBudget,
    SearchManifest, SearchManifestId};

fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn bytes(owner: ArenaOwnerId, revision: u64, bytes: Vec<u8>) -> SourceCapture {
    SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, revision).unwrap(), "memory.rs", bytes).unwrap()
}
fn budget(owner: ArenaOwnerId) -> ResourceBudget {
    ResourceBudget::new(owner, ByteLength::new(4 * 1024 * 1024)).unwrap()
}

struct ChangingProvider { owner: ArenaOwnerId, calls: AtomicUsize }
impl SourceProvider for ChangingProvider {
    fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(bytes(self.owner, call as u64 + 1,
            if call == 0 { b"old needle".to_vec() } else { b"new source".to_vec() }))
    }
}

#[test]
fn public_facade_search_opens_the_searched_revision_not_changed_live_source() {
    let owner = ArenaOwnerId::new(91).unwrap();
    let provider = Arc::new(ChangingProvider { owner, calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner, provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(session.require_feature(Feature::Search), Ok(()));
    let prepared = session.capture_for_search("memory.rs").unwrap();
    assert_eq!(prepared.source().bytes().as_ptr(), prepared.capture().bytes().as_ptr());
    let docs = [prepared.document()];
    let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &docs,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let budget = budget(owner);
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let query = ParsedQuery::parse("needle").unwrap();
    let generation = QueryGeneration::new(owner, 1).unwrap();
    let report = index.search(&query, QueryOptions::new(generation), &budget, allocation(2), || false).unwrap();
    report.validate_delivery(manifest.id(), generation).unwrap();
    assert!(report.is_complete());
    let hit = &report.capture_results().matches[0];
    let live = session.open("memory.rs").unwrap();
    assert_eq!(live.source().revision().get(), 2);
    assert_eq!(live.search_hit_bytes(hit), Err(FcbError::StaleGeneration));
    let selected = session.open_search_hit(&prepared, hit).unwrap();
    assert_eq!(selected.source().revision().get(), 1);
    assert_eq!(selected.search_hit_bytes(hit).unwrap(), b"needle");
    assert_eq!(selected.source().bytes().as_ptr(), prepared.capture().bytes().as_ptr());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(selected.frame_plan().unwrap().file(), hit.file_id);
    assert_eq!(selected.frame_plan().unwrap().source(), hit.revision);
}

#[test]
fn host_supplied_capture_needs_no_provider_runtime_or_store() {
    let owner = ArenaOwnerId::new(92).unwrap();
    let session = BrowserSession::new(owner);
    let source = bytes(owner, 1, b"banana".to_vec());
    let prepared = session.prepare_search_capture(source).unwrap();
    let docs = [prepared.document()];
    let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &docs,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let budget = budget(owner);
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let query = ParsedQuery::parse("ana").unwrap();
    let report = index.search(&query, QueryOptions::new(QueryGeneration::new(owner, 1).unwrap()),
        &budget, allocation(2), || false).unwrap();
    assert_eq!(report.capture_results().matches.len(), 2);
    for hit in &report.capture_results().matches {
        assert_eq!(prepared.hit_bytes(hit).unwrap(), b"ana");
        assert_eq!(session.open_search_hit(&prepared, hit).unwrap().search_hit_bytes(hit).unwrap(), b"ana");
    }
    assert!(!session.available_features().contains(Feature::Runtime));
    assert!(!session.available_features().contains(Feature::Persistence));
    assert!(matches!(session.capture_for_search("not-granted.rs"), Err(FcbError::ProviderUnavailable)));
}

#[test]
fn source_owner_revision_file_and_range_are_checked_before_navigation() {
    let owner = ArenaOwnerId::new(93).unwrap();
    let session = BrowserSession::new(owner);
    let prepared = session.prepare_search_capture(bytes(owner, 1, b"needle".to_vec())).unwrap();
    let docs = [prepared.document()];
    let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &docs,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let budget = budget(owner);
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let query = ParsedQuery::parse("needle").unwrap();
    let report = index.search(&query, QueryOptions::new(QueryGeneration::new(owner, 1).unwrap()),
        &budget, allocation(2), || false).unwrap();
    let valid = report.capture_results().matches[0].clone();
    let foreign_owner = ArenaOwnerId::new(94).unwrap();
    let foreign_session = BrowserSession::new(foreign_owner);
    assert_eq!(foreign_session.open_search_hit(&prepared, &valid), Err(FcbError::OwnerMismatch));
    let mut hit = valid.clone(); hit.revision = SourceRevision::new(owner, 2).unwrap();
    assert_eq!(session.open_search_hit(&prepared, &hit), Err(FcbError::StaleGeneration));
    hit = valid.clone(); hit.file_id = FileId::new(owner, 2).unwrap();
    assert_eq!(session.open_search_hit(&prepared, &hit), Err(FcbError::SourceNotFound));
    hit = valid.clone(); hit.file_id = FileId::new(foreign_owner, 1).unwrap();
    assert_eq!(session.open_search_hit(&prepared, &hit), Err(FcbError::OwnerMismatch));
    hit = valid.clone(); hit.original_byte_range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(100)).unwrap();
    assert_eq!(prepared.hit_bytes(&hit), Err(FcbError::SourceNotFound));
    hit = valid; hit.original_byte_range = ByteRange::new(ByteOffset::new(2), ByteOffset::new(2)).unwrap();
    assert_eq!(prepared.hit_bytes(&hit), Err(FcbError::SourceNotFound));
    assert!(matches!(session.prepare_search_capture(bytes(foreign_owner, 1, b"foreign".to_vec())), Err(FcbError::OwnerMismatch)));
}

#[test]
fn utf16_result_navigation_copies_original_units_not_decoded_utf8_offsets() {
    let owner = ArenaOwnerId::new(95).unwrap();
    let session = BrowserSession::new(owner);
    let mut raw = vec![0xff, 0xfe];
    for unit in "go \u{1f680}".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
    let prepared = session.prepare_search_capture(bytes(owner, 1, raw)).unwrap();
    let docs = [prepared.document()];
    let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &docs,
        &[], MembershipState::Closed, ManifestLimits::default()).unwrap();
    let budget = budget(owner);
    let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget, allocation(1), || false).unwrap();
    let query = ParsedQuery::parse("\u{1f680}").unwrap();
    let report = index.search(&query, QueryOptions::new(QueryGeneration::new(owner, 1).unwrap()),
        &budget, allocation(2), || false).unwrap();
    let hit = &report.capture_results().matches[0];
    assert_eq!(hit.original_byte_range.start().get(), 8);
    assert_eq!(hit.decoded_range.unwrap().start().get(), 3);
    assert_eq!(prepared.hit_bytes(hit).unwrap(), &[0x3d, 0xd8, 0x80, 0xde]);
    assert_eq!(session.open_search_hit(&prepared, hit).unwrap().search_hit_bytes(hit).unwrap(), &[0x3d, 0xd8, 0x80, 0xde]);
}

#[test]
fn closing_one_session_leaves_another_session_and_retained_search_source_alive() {
    let owner = ArenaOwnerId::new(96).unwrap();
    let first = BrowserSession::new(owner);
    let second = BrowserSession::new(ArenaOwnerId::new(97).unwrap());
    let prepared = first.prepare_search_capture(bytes(owner, 1, b"retained".to_vec())).unwrap();
    first.close();
    assert_eq!(prepared.capture().bytes(), b"retained");
    let independent = second.prepare_search_capture(bytes(second.owner(), 1, b"independent".to_vec())).unwrap();
    assert_eq!(independent.capture().bytes(), b"independent");
    assert_ne!(prepared.capture().request().file(), independent.capture().request().file());
}

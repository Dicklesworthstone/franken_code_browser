#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Public consumer scenarios: no mock reader/index/decoder, no filesystem and
//! no native renderer. A changing provider makes accidental recapture fail.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FcbError, FileId,
    SourceCapture, SourceProvider, SourceRevision};
use fcb::search::{DirectSourceScanner, MembershipState, NativeSourceIdentity, PathEntry,
    PathIndex, PathIndexLimits, PathNavigationTarget, PathSearch, PathSearchOptions,
    QueryGeneration, QueryOptions, RawPath, ReaderError, ReaderLimits, ReadingAnchor,
    ReadingSeekState, ReadingTarget, ReadingWindowOptions, ResourceAllocationId,
    ResourceBudget, RootId, SearchManifestId};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(819).unwrap() }
fn file(id: u64) -> FileId { FileId::new(owner(), id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn source(id: u64, revision: u64, path: &str, bytes: Vec<u8>) -> SourceCapture {
    SourceCapture::from_bytes(owner(), file(id), SourceRevision::new(owner(), revision).unwrap(), path, bytes).unwrap()
}
fn ready(seek: &mut fcb::search::ReadingSeek<'_>) -> ReadingAnchor {
    loop {
        match seek.state() {
            ReadingSeekState::Ready(anchor) => return anchor,
            ReadingSeekState::Pending => { seek.step(4, seek.generation(), || false).unwrap(); }
            state => panic!("unexpected reading state {state:?}"),
        }
    }
}
struct ChangingProvider { calls: AtomicUsize }
impl SourceProvider for ChangingProvider {
    fn capture(&self, path: &str) -> Result<SourceCapture, FcbError> {
        assert_eq!(path, "src/Thing.rs");
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut bytes = vec![0xff, 0xfe];
        for unit in (if call == 0 { "head\r\nalpha needle 😀\r\nlast" } else { "a completely different file" }).encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        Ok(source(1, 7 + call as u64, path, bytes))
    }
}

#[test]
fn content_search_opens_reads_selects_and_frames_the_exact_searched_utf16_capture() {
    let provider = Arc::new(ChangingProvider { calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner(), provider.clone());
    let prepared = session.capture_for_search("src/Thing.rs").unwrap();
    let result = DirectSourceScanner::scan_complete_capture(prepared.capture(), "needle", &QueryOptions::new(generation(1))).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.matches.len(), 1);
    let hit = &result.matches[0];
    let view = session.open_search_hit(&prepared, hit).unwrap();
    let budget = budget();
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let mut seek = reader.seek_hit(hit, generation(1)).unwrap();
    let at = ready(&mut seek);
    assert_eq!(at.line_number(), 2);
    assert_eq!(at.line_start().get(), 14);
    assert_eq!(at.offset().get(), 26);
    assert_eq!(at.selection(), Some(hit.original_byte_range));
    let window = reader.window(at, generation(1), ReadingWindowOptions { max_bytes: 64, max_lines: 1 },
        &budget, allocation(2), || false).unwrap();
    assert_eq!(window.line_text(0), Some("needle 😀"));
    assert!(window.lines()[0].continued_before);
    let selection = window.text_selection(window.source_to_text(hit.original_byte_range).unwrap()).unwrap();
    assert_eq!(selection.text, "needle");
    assert_eq!(selection.original_bytes, prepared.hit_bytes(hit).unwrap());
    assert_eq!(selection.original_bytes.as_ptr(), prepared.hit_bytes(hit).unwrap().as_ptr());
    assert_eq!(window.frame_plan().file(), file(1));
    assert_eq!(window.frame_plan().source().get(), 7);
    assert_eq!(window.frame_plan().bytes().start().get(), 26);
    let next = window.next_anchor().unwrap();
    drop(window);
    let last = reader.window(next, generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
    assert_eq!(last.lines()[0].number, 3);
    assert_eq!(last.line_text(0), Some("last"));
    assert!(last.reaches_eof());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1, "reading must not fetch changed working bytes");
}

#[test]
fn metadata_path_search_opens_only_the_selected_file_then_prepares_reading_rows() {
    let budget = budget();
    let paths = [RawPath::from_str("src/Thing.rs"), RawPath::from_str("src/thing.rs")];
    let root = RootId::new(owner(), 1).unwrap();
    let index_id = SearchManifestId::new(owner(), 1).unwrap();
    let entries = [PathEntry::new(file(1), root, &paths[0]), PathEntry::new(file(2), root, &paths[1])];
    let index = PathIndex::build(index_id, MembershipState::Closed, &entries, PathIndexLimits::default(),
        &budget, allocation(1), || false).unwrap();
    let provider = Arc::new(ChangingProvider { calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner(), provider.clone());
    let mut query = PathSearch::new(&index, b"Thing.rs", PathSearchOptions::new(generation(1)), &budget, allocation(2)).unwrap();
    query.run_to_completion(|| false).unwrap();
    assert_eq!(query.matches_seen(), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    query.select(file(1)).unwrap();
    let target = PathNavigationTarget::from_search(&query, file(1)).unwrap();
    let capture = provider.capture("src/Thing.rs").unwrap();
    let view = session.open_path_target(&target, index_id, generation(1),
        NativeSourceIdentity { root, path: &paths[0] }, capture).unwrap();
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(3)).unwrap();
    let mut seek = reader.seek(ReadingTarget::Byte(ByteOffset::new(0)), generation(2)).unwrap();
    let window = reader.window(ready(&mut seek), generation(2), ReadingWindowOptions::default(),
        &budget, allocation(4), || false).unwrap();
    assert_eq!(window.line_text(0), Some("head"));
    assert_eq!(window.line_text(1), Some("alpha needle 😀"));
    assert_eq!(window.line_text(2), Some("last"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_search_hits_and_later_reader_requests_cannot_relabel_new_bytes() {
    let provider = Arc::new(ChangingProvider { calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner(), provider.clone());
    let prepared = session.capture_for_search("src/Thing.rs").unwrap();
    let result = DirectSourceScanner::scan_complete_capture(prepared.capture(), "needle", &QueryOptions::new(generation(1))).unwrap();
    let hit = &result.matches[0];
    let view = session.open_search_hit(&prepared, hit).unwrap();
    let budget = budget();
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let mut seek = reader.seek_hit(hit, generation(1)).unwrap();
    let at = ready(&mut seek);
    let retained = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
    let changed = session.open("src/Thing.rs").unwrap(); // Explicit refresh, not a hidden read.
    let next = changed.source_reader(ReaderLimits::default(), &budget, allocation(3)).unwrap();
    assert!(matches!(next.seek_hit(hit, generation(2)), Err(ReaderError::StaleSource)));
    assert_eq!(retained.validate_delivery(changed.source(), generation(1)), Err(ReaderError::StaleSource));
    assert_eq!(retained.validate_delivery(view.source(), generation(2)), Err(ReaderError::StaleQuery));
    assert_eq!(retained.line_text(0), Some("needle 😀"));
    assert_eq!(retained.frame_plan().source().get(), 7);
    assert_eq!(changed.source().revision().get(), 8);
}

#[test]
fn raw_search_of_invalid_bytes_remains_byte_exact_through_reader_selection() {
    let session = BrowserSession::new(owner());
    let prepared = session.prepare_search_capture(source(1, 1, "raw.rs", b"head\r\na\xffz\n".to_vec())).unwrap();
    let options = QueryOptions::new(generation(1));
    let result = DirectSourceScanner::scan_raw_bytes(prepared.source().bytes(), file(1), prepared.source().revision(), &[0xff], &options).unwrap();
    let hit = &result.matches[0];
    let view = session.open_search_hit(&prepared, hit).unwrap();
    let budget = budget();
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(1)).unwrap();
    let mut seek = reader.seek_hit(hit, generation(1)).unwrap();
    let at = ready(&mut seek);
    assert_eq!(at.line_number(), 2);
    let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(2), || false).unwrap();
    assert!(window.has_replacements());
    assert_eq!(window.line_text(0), Some("\u{fffd}z"));
    let selection = window.text_selection(window.source_to_text(hit.original_byte_range).unwrap()).unwrap();
    assert!(selection.contains_replacements);
    assert_eq!(selection.original_bytes, &[0xff]);
    assert_eq!(reader.raw_selection(ByteRange::new(ByteOffset::new(0), ByteOffset::new(10)).unwrap(), 10).unwrap(), b"head\r\na\xffz\n");
}
